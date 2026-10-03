#!/usr/bin/env python3
"""Release-backed CI image contract with a real disposable Distribution registry."""
import contextlib
import gzip
import hashlib
import http.server
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tarfile
import tempfile
import threading
import time
import tomllib
import unittest
import urllib.error
import urllib.request

import yaml

spec = importlib.util.spec_from_file_location('base_image', Path(__file__).with_name('ci-base-image.py'))
image = importlib.util.module_from_spec(spec)
spec.loader.exec_module(image)


def encoded(value):
    return json.dumps(value, separators=(',', ':')).encode()


def fixture(path):
    path.mkdir()
    archive = io.BytesIO()
    with tarfile.open(fileobj=archive, mode='w') as stream:
        data = b'reviewed inert fixture\n'
        member = tarfile.TarInfo('proof.txt')
        member.size = len(data)
        member.mode = 0o644
        stream.addfile(member, io.BytesIO(data))
    layer = gzip.compress(archive.getvalue(), mtime=0)
    config = encoded({'architecture': 'amd64', 'os': 'linux', 'config': {},
                      'rootfs': {'type': 'layers', 'diff_ids': ['sha256:' + hashlib.sha256(archive.getvalue()).hexdigest()]}})

    def blob(data, kind):
        digest = hashlib.sha256(data).hexdigest()
        (path / digest).write_bytes(data)
        return {'digest': 'sha256:' + digest, 'size': len(data), 'mediaType': kind}

    manifest = encoded({'schemaVersion': 2, 'mediaType': 'application/vnd.oci.image.manifest.v1+json',
                        'config': blob(config, 'application/vnd.oci.image.config.v1+json'),
                        'layers': [blob(layer, 'application/vnd.oci.image.layer.v1.tar+gzip')]})
    (path / 'manifest.json').write_bytes(manifest)
    (path / 'version').write_bytes(b'Directory Transport Version: 1.1\n')
    digest = hashlib.sha256(manifest).hexdigest()
    release_archive = path.parent / 'fixture.tar'
    archive_layout(path, release_archive)
    return {'source': f'origin.example/source:reviewed@sha256:{digest}',
            'local': f'127.0.0.1:55071/reviewed-image@sha256:{digest}',
            'platform': 'linux/amd64',
            'release': {'repository': 'FieldmouseWorks/Conary', 'tag': 'fixture-v1',
                        'asset': 'fixture.tar', 'sha256': hashlib.sha256(release_archive.read_bytes()).hexdigest(),
                        'size': release_archive.stat().st_size}}


def archive_layout(layout, archive, extra=(), omit=()):
    with tarfile.open(archive, mode='w') as stream:
        for path in sorted(layout.iterdir()):
            if path.name in omit:
                continue
            data = path.read_bytes()
            member = tarfile.TarInfo(path.name)
            member.size = len(data)
            stream.addfile(member, io.BytesIO(data))
        for member, data in extra:
            stream.addfile(member, io.BytesIO(data) if member.isfile() else None)


@contextlib.contextmanager
def registry(root):
    root.mkdir()
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    config = root / 'config.json'
    config.write_text(json.dumps({'version': 0.1, 'log': {'level': 'error'},
                                 'storage': {'filesystem': {'rootdirectory': str(root / 'store')},
                                             'delete': {'enabled': True}},
                                 'http': {'addr': f'127.0.0.1:{port}', 'secret': 'disposable-fixture'}}))
    with (root / 'registry.log').open('wb') as log:
        process = subprocess.Popen([os.environ.get('CONARY_REGISTRY_BINARY', 'docker-registry'),
                                    'serve', str(config)], stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 10
            while True:
                try:
                    with urllib.request.urlopen(f'http://127.0.0.1:{port}/v2/', timeout=1):
                        break
                except OSError:
                    if process.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError((root / 'registry.log').read_text())
                    time.sleep(0.05)
            yield f'127.0.0.1:{port}'
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)


@contextlib.contextmanager
def release_server(archive, entry):
    requests = []
    metadata = {'tag_name': entry['release']['tag'], 'immutable': True,
                'draft': False, 'prerelease': False, 'assets': []}

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            requests.append((self.path, self.headers.get('Authorization')))
            if self.path.startswith('/repos/'):
                body = encoded(metadata)
            elif self.path.startswith('/FieldmouseWorks/'):
                body = archive.read_bytes()
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_args):
            pass

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    base = f'http://127.0.0.1:{server.server_port}'
    _, url = image.release_urls(entry['release'], base, base)
    metadata['assets'] = [{'name': entry['release']['asset'],
                           'digest': 'sha256:' + entry['release']['sha256'],
                           'size': entry['release']['size'], 'state': 'uploaded',
                           'browser_download_url': url}]
    try:
        yield base, metadata, requests
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


class RetentionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='conary-retention-test-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.layout = self.root / 'source'
        self.entry = fixture(self.layout)

    def test_exact_content_and_reference_binding(self):
        receipt = image.verify(self.layout, self.entry)
        self.assertEqual(len(receipt['blobs']), 2)
        entries = {'fixture': self.entry}
        self.assertIn('@sha256:', image.resolve(self.entry['local'], entries))
        with self.assertRaisesRegex(ValueError, 'local reference'):
            image.resolve(self.entry['source'], entries)
        with self.assertRaisesRegex(ValueError, 'local reference'):
            image.resolve(self.entry['source'].replace(':reviewed@', '@'), entries)
        with self.assertRaises(ValueError):
            image.reference('retained.example/image:latest')
        with self.assertRaises(ValueError):
            image.reference('implicit/image@sha256:' + '0' * 64)
        with self.assertRaises(ValueError):
            image.reference('retained.example/../image@sha256:' + '0' * 64)
        for value in ('retained.example/image', 'localhost/image', '0.0.0.0/image'):
            with self.assertRaises(ValueError):
                image.transport_flags(value + '@sha256:' + '0' * 64, 'src', True)
        with self.assertRaisesRegex(ValueError, 'not registered'):
            image.resolve('127.0.0.1:5001/other@sha256:' + '0' * 64, entries)

    def test_corrupt_manifest_config_and_layer_fail_independently(self):
        for path in self.layout.iterdir():
            if path.name == 'version':
                continue
            with self.subTest(name=path.name):
                original = path.read_bytes()
                path.write_bytes(bytes([original[0] ^ 1]) + original[1:])
                with self.assertRaisesRegex(ValueError, 'digest mismatch'):
                    image.verify(self.layout, self.entry)
                path.write_bytes(original)

    def test_descriptor_and_platform_authority_fail_independently(self):
        with self.assertRaisesRegex(ValueError, 'platform does not match'):
            image.verify(self.layout, {**self.entry, 'platform': 'linux/arm64'})
        manifest_path = self.layout / 'manifest.json'
        original = manifest_path.read_bytes()
        manifest = json.loads(original)
        for label, update, error in (
            ('external-url', lambda m: m['layers'][0].update(urls=['https://evil.example/blob']),
             'external layer URLs'),
            ('wrong-size', lambda m: m['layers'][0].update(size=m['layers'][0]['size'] + 1),
             'blob size mismatch'),
            ('index', lambda m: m.update(mediaType='application/vnd.oci.image.index.v1+json'),
             'pinned platform manifest'),
        ):
            with self.subTest(label=label):
                changed = json.loads(original)
                update(changed)
                payload = encoded(changed)
                manifest_path.write_bytes(payload)
                digest = hashlib.sha256(payload).hexdigest()
                entry = {**self.entry,
                         'source': f'origin.example/source@sha256:{digest}',
                         'local': f'127.0.0.1:55071/reviewed-image@sha256:{digest}'}
                with self.assertRaisesRegex(ValueError, error):
                    image.verify(self.layout, entry)
        manifest_path.write_bytes(original)

    def test_directory_transport_version_is_required_and_exact(self):
        version = self.layout / 'version'
        original = version.read_bytes()
        version.unlink()
        with self.assertRaisesRegex(ValueError, 'missing directory transport version'):
            image.verify(self.layout, self.entry)
        missing_archive = self.root / 'missing-version.tar'
        archive_layout(self.layout, missing_archive)
        with self.assertRaisesRegex(ValueError, 'missing directory transport version'):
            image.restore(missing_archive, self.root / 'missing-version', self.entry)

        version.write_bytes(original.replace(b'1.1', b'1.0'))
        with self.assertRaisesRegex(ValueError, 'invalid directory transport version'):
            image.verify(self.layout, self.entry)
        changed_archive = self.root / 'changed-version.tar'
        archive_layout(self.layout, changed_archive)
        with self.assertRaisesRegex(ValueError, 'invalid directory transport version'):
            image.restore(changed_archive, self.root / 'changed-version', self.entry)
        version.write_bytes(original)

    def test_missing_truncated_symlinked_and_wrong_authority_fail(self):
        path = next(p for p in self.layout.iterdir() if p.name not in ('manifest.json', 'version'))
        original = path.read_bytes()
        path.unlink()
        with self.assertRaises(FileNotFoundError):
            image.verify(self.layout, self.entry)
        path.write_bytes(original[:-1])
        with self.assertRaisesRegex(ValueError, 'size mismatch'):
            image.verify(self.layout, self.entry)
        path.unlink()
        outside = self.root / 'outside'
        outside.write_bytes(original)
        path.symlink_to(outside)
        with self.assertRaisesRegex(ValueError, 'regular blob'):
            image.verify(self.layout, self.entry)
        path.unlink()
        path.write_bytes(original)
        wrong = {**self.entry, 'source': 'origin.example/source@sha256:' + '0' * 64}
        with self.assertRaisesRegex(ValueError, 'digest mismatch'):
            image.verify(self.layout, wrong)

    def test_catalog_rejects_changed_digest_and_platform(self):
        path = self.root / 'catalog.json'
        for change in ({'local': '127.0.0.1:55071/image@sha256:' + '0' * 64},
                       {'local': 'registry.example/image@sha256:' + image.reference(self.entry['source'])[1]},
                       {'platform': 'linux/arm64'}, {'unexpected': 'value'},
                       {'release': {**self.entry['release'], 'sha256': '0' * 63}},
                       {'release': {**self.entry['release'], 'asset': '../fixture.tar'}},
                       {'release': {**self.entry['release'], 'size': 0}}):
            path.write_text(json.dumps({'version': 2, 'images': {'fixture': {**self.entry, **change}}}))
            with self.assertRaises(ValueError):
                image.catalog(path)

    def test_shipped_local_reference_matches_typed_image_configuration(self):
        root = Path(__file__).resolve().parent.parent
        config = tomllib.loads((root / 'apps/conary/tests/integration/remi/config.toml').read_text())
        for name, entry in image.catalog(image.CATALOG).items():
            self.assertEqual(config['distros'][name]['target_root']['base_image'],
                             entry['local'])

    def test_restore_exact_archive_and_reject_changed_bytes(self):
        archive = self.root / 'image.tar'
        archive_layout(self.layout, archive)
        restored = self.root / 'restored'
        receipt = image.restore(archive, restored, self.entry)
        self.assertEqual(receipt['manifest_sha256'], image.reference(self.entry['source'])[1])
        self.assertEqual({p.name: p.read_bytes() for p in restored.iterdir()},
                         {p.name: p.read_bytes() for p in self.layout.iterdir()})
        self.assertEqual(image.verify(restored, self.entry), receipt)

        blob = next(p for p in self.layout.iterdir() if p.name not in ('manifest.json', 'version'))
        original = blob.read_bytes()
        blob.write_bytes(original[:-1] + bytes([original[-1] ^ 1]))
        archive_layout(self.layout, archive)
        failed = self.root / 'changed'
        with self.assertRaisesRegex(ValueError, 'digest mismatch'):
            image.restore(archive, failed, self.entry)
        self.assertFalse(failed.exists())

    def test_restore_rejects_extra_missing_duplicate_links_and_traversal(self):
        cases = []
        extra = tarfile.TarInfo('extra.txt')
        extra.size = 1
        cases.append(('extra', [(extra, b'x')], (), 'unexpected archive member'))
        extra = tarfile.TarInfo('f' * 64)
        extra.size = 1
        cases.append(('extra-digest', [(extra, b'x')], (), 'unexpected image-directory content'))
        extra = tarfile.TarInfo('manifest.json')
        extra.size = 1
        cases.append(('duplicate', [(extra, b'x')], (), 'duplicate archive member'))
        for name in ('../outside', '/outside', './manifest.json', 'nested/blob'):
            extra = tarfile.TarInfo(name)
            extra.size = 1
            cases.append((name.replace('/', '_'), [(extra, b'x')], (), 'unexpected archive member'))
        for kind, label in ((tarfile.SYMTYPE, 'symlink'), (tarfile.LNKTYPE, 'hardlink')):
            extra = tarfile.TarInfo('version')
            extra.type = kind
            extra.linkname = '../outside'
            cases.append((label, [(extra, b'')], ('version',), 'non-regular archive member'))
        blob = next(p for p in self.layout.iterdir() if p.name not in ('manifest.json', 'version'))
        cases.append(('missing', [], (blob.name,), 'No such file or directory'))
        cases.append(('trailing', [], (), 'nonzero bytes after archive members'))
        for label, extra, omit, error in cases:
            with self.subTest(label=label):
                archive = self.root / f'{label}.tar'
                archive_layout(self.layout, archive, extra=extra, omit=omit)
                if label == 'trailing':
                    with archive.open('ab') as stream:
                        stream.write(b'concealed extra member'.ljust(512, b'\0'))
                destination = self.root / f'{label}-restored'
                with self.assertRaisesRegex((ValueError, FileNotFoundError), error):
                    image.restore(archive, destination, self.entry)
                self.assertFalse(destination.exists())
                self.assertFalse((self.root / 'outside').exists())
        archive = self.root / 'regular.tar'
        archive_layout(self.layout, archive)
        symlink = self.root / 'linked-archive.tar'
        symlink.symlink_to(archive)
        with self.assertRaisesRegex(ValueError, 'regular OCI dir archive'):
            image.restore(symlink, self.root / 'symlink-restored', self.entry)

    def test_release_metadata_and_archive_refuse_changed_authority(self):
        archive = self.root / 'fixture.tar'
        changes = (
            ('mutable', lambda m: m.update(immutable=False)),
            ('draft', lambda m: m.update(draft=True)),
            ('prerelease', lambda m: m.update(prerelease=True)),
            ('tag', lambda m: m.update(tag_name='other')),
            ('duplicate', lambda m: m['assets'].append(dict(m['assets'][0]))),
            ('asset-digest', lambda m: m['assets'][0].update(digest='sha256:' + '0' * 64)),
            ('asset-size', lambda m: m['assets'][0].update(size=1)),
            ('asset-url', lambda m: m['assets'][0].update(browser_download_url='https://evil.example/asset')),
        )
        for label, change in changes:
            with self.subTest(label=label), release_server(archive, self.entry) as (base, metadata, requests):
                change(metadata)
                destination = self.root / f'{label}.tar'
                with self.assertRaises(ValueError):
                    image.download_release(self.entry, destination, base, base)
                self.assertFalse(destination.exists())
                self.assertEqual(len(requests), 1)
                self.assertIsNone(requests[0][1])
        with release_server(archive, self.entry) as (base, _metadata, requests):
            original = archive.read_bytes()
            archive.write_bytes(original[:-1] + bytes([original[-1] ^ 1]))
            try:
                destination = self.root / 'changed-archive.tar'
                with self.assertRaisesRegex(ValueError, 'SHA-256 mismatch'):
                    image.download_release(self.entry, destination, base, base)
                self.assertFalse(destination.exists())
                self.assertEqual(len(requests), 2)
                self.assertTrue(all(auth is None for _, auth in requests))
            finally:
                archive.write_bytes(original)

    def test_cold_release_to_local_registry_survives_deleted_sources(self):
        # Two real Distribution registries; no host image daemon or image execution.
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        _, digest = image.reference(self.entry['source'])
        local = f'127.0.0.1:{port}/reviewed-image@sha256:{digest}'
        workdir = self.root / 'prepared'
        archive = self.root / 'fixture.tar'
        with registry(self.root / 'origin-registry') as origin:
            entry = {**self.entry, 'source': f'{origin}/source@sha256:{digest}',
                     'local': local}
            image.copy_image(f'dir:{self.layout}', f'docker://{origin}/source:reviewed',
                             ['--dest-no-creds', '--dest-tls-verify=false'])
            origin_url = f'http://{origin}/v2/source/manifests/sha256:{digest}'
            manifest_request = urllib.request.Request(
                origin_url, headers={'Accept': 'application/vnd.oci.image.manifest.v1+json'})
            with urllib.request.urlopen(manifest_request, timeout=5) as response:
                self.assertEqual(response.status, 200)
            with urllib.request.urlopen(
                    urllib.request.Request(origin_url, method='DELETE'), timeout=5) as response:
                self.assertEqual(response.status, 202)
            with self.assertRaises(urllib.error.HTTPError) as failure:
                urllib.request.urlopen(manifest_request, timeout=5)
            self.assertEqual(failure.exception.code, 404)
            failure.exception.close()

            with release_server(archive, entry) as (base, _metadata, requests):
                receipt = image.prepare(entry, workdir, base, base)
            try:
                self.assertEqual(receipt['manifest_sha256'], digest)
                self.assertEqual(receipt['archive_sha256'], entry['release']['sha256'])
                self.assertEqual(receipt['local_reference'], entry['local'])
                self.assertFalse((workdir / 'layout').exists())
                self.assertFalse((workdir / 'fixture.tar').exists())
                self.assertEqual(len(requests), 2)
                self.assertTrue(all(auth is None for _, auth in requests))
                shutil.rmtree(self.layout)
                archive.unlink()
                fetched = image.fetch_local(entry, self.root / 'cold-read')
                self.assertEqual(fetched['manifest_sha256'], digest)
                self.assertEqual(len(fetched['blobs']), 2)
                self.assertFalse(self.layout.exists())
                self.assertFalse(archive.exists())
            finally:
                image.cleanup(workdir)
            self.assertFalse(workdir.exists())
            # An available origin still cannot replace the stopped local transport.
            image.copy_image(f'dir:{self.root / "cold-read"}',
                             f'docker://{origin}/source:reviewed',
                             ['--dest-no-creds', '--dest-tls-verify=false'])
            with urllib.request.urlopen(manifest_request, timeout=5) as response:
                self.assertEqual(response.status, 200)
            with self.assertRaises(subprocess.CalledProcessError):
                image.fetch_local(entry, self.root / 'stopped-registry')

    def test_failed_release_prepare_removes_its_owned_workdir(self):
        workdir = self.root / 'failed-prepare'
        with release_server(self.root / 'fixture.tar', self.entry) as (base, metadata, _requests):
            metadata['immutable'] = False
            with self.assertRaisesRegex(ValueError, 'immutable reviewed release'):
                image.prepare(self.entry, workdir, base, base)
        self.assertFalse(workdir.exists())
        image.cleanup(workdir)
        self.assertFalse(workdir.exists())

    def test_prepare_cli_stdout_is_json_and_registry_survives_next_process(self):
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        _, digest = image.reference(self.entry['source'])
        entry = {**self.entry, 'local': f'127.0.0.1:{port}/reviewed-image@sha256:{digest}'}
        catalog_path = self.root / 'catalog.json'
        catalog_path.write_text(json.dumps({'version': 2, 'images': {'fixture': entry}}))
        workdir = self.root / 'cli-prepared'
        wrapper = '''
import importlib.util
import sys
spec = importlib.util.spec_from_file_location('base_image', sys.argv[2])
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
real_urls = helper.release_urls
base = sys.argv[1]
helper.release_urls = lambda release, api_base='https://api.github.com', download_base='https://github.com': real_urls(release, base, base)
sys.argv = [sys.argv[2], '--catalog', sys.argv[3], 'prepare', '--image', 'fixture', '--workdir', sys.argv[4]]
helper.main()
'''
        with release_server(self.root / 'fixture.tar', entry) as (base, _metadata, requests):
            try:
                completed = subprocess.run(
                    ['python3', '-c', wrapper, base, str(Path(image.__file__)),
                     str(catalog_path), str(workdir)],
                    capture_output=True, text=True, check=True, timeout=60)
                receipt = json.loads(completed.stdout)
                self.assertEqual(receipt['manifest_sha256'], digest)
                self.assertEqual(receipt['archive_sha256'], entry['release']['sha256'])
                self.assertEqual(receipt['local_reference'], entry['local'])
                self.assertEqual(len(requests), 2)
                self.assertTrue(all(auth is None for _, auth in requests))
                url = f'http://127.0.0.1:{port}/v2/reviewed-image/manifests/sha256:{digest}'
                request = urllib.request.Request(
                    url, headers={'Accept': 'application/vnd.oci.image.manifest.v1+json'})
                with urllib.request.urlopen(request, timeout=5) as response:
                    self.assertEqual(hashlib.sha256(response.read()).hexdigest(), digest)
            finally:
                image.cleanup(workdir)
        self.assertFalse(workdir.exists())

    def test_local_registry_refuses_an_occupied_port(self):
        workdir = self.root / 'occupied'
        workdir.mkdir()
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            host, port = listener.getsockname()
            listener.listen()
            entry = {**self.entry, 'local': self.entry['local'].replace('127.0.0.1:55071',
                                                                         f'{host}:{port}')}
            with self.assertRaises(OSError):
                image.start_registry(entry, workdir)
        self.assertFalse((workdir / 'registry.pid').exists())


class ConsumerWorkflowTests(unittest.TestCase):
    def test_release_catalog_pins_the_exact_public_asset(self):
        root = Path(__file__).resolve().parent.parent
        entry = image.catalog(root / 'scripts/ci-base-images.json')['opensuse-tumbleweed']
        self.assertEqual(entry['local'],
                         '127.0.0.1:55071/conary-ci-base-opensuse-tumbleweed@sha256:'
                         '000f41d72d21563074c380c3f5d27c6d28c9f8aa5f593afaa923905e70c4a0f2')
        self.assertEqual(entry['release'], {
            'repository': 'FieldmouseWorks/Conary',
            'tag': 'ci-base-971-tumbleweed-20260908',
            'asset': 'opensuse-tumbleweed-20260908-oci-dir.tar',
            'sha256': 'db4dce3794bc93d307dacac63905ab3c11d360a22b24b1cf0dce97f5e38e2824',
            'size': 42803200,
        })
        self.assertFalse((root / '.github/workflows/publish-ci-base-image.yml').exists())

    def test_release_action_prepares_before_uncached_anonymous_digest_pull(self):
        root = Path(__file__).resolve().parent.parent
        action = yaml.load((root / '.github/actions/cache-base-image/action.yml').read_text(),
                           Loader=yaml.BaseLoader)
        steps = action['runs']['steps']
        named = {step.get('name'): step for step in steps}
        prepare = named['Restore reviewed release into the local registry']
        pull = named['Require the reviewed registry image by digest']
        self.assertEqual(prepare['if'], "${{ steps.base-image.outputs.release-backed == 'true' }}")
        self.assertIn('ci-base-image.py" prepare', prepare['run'])
        self.assertLess(steps.index(prepare), steps.index(pull))
        self.assertNotIn('if', pull)
        self.assertIn('DOCKER_CONFIG="$pull_config" docker pull "$BASE_IMAGE_REF"', pull['run'])
        self.assertIn('docker image inspect "$BASE_IMAGE_REF"', pull['run'])
        for name in ('Restore the cached base image', 'Load the cached base image',
                     'Save the pulled base image into the cache'):
            self.assertIn("steps.base-image.outputs.release-backed != 'true'", named[name]['if'])
        self.assertNotIn('GITHUB_TOKEN', str(action))
        self.assertNotIn('GH_TOKEN', str(action))

    def test_release_proof_checkout_includes_trusted_helper_and_catalog(self):
        root = Path(__file__).resolve().parent.parent
        workflow = yaml.load((root / '.github/workflows/release-artifact-proof.yml').read_text(),
                             Loader=yaml.BaseLoader)
        job = workflow['jobs']['native-package-lifecycle']
        checkout = next(step for step in job['steps']
                        if step.get('name') == 'Check out workflow authority for local actions')
        self.assertEqual(checkout['with']['ref'], '${{ github.workflow_sha }}')
        self.assertEqual(checkout['with']['sparse-checkout-cone-mode'], 'false')
        self.assertEqual(set(checkout['with']['sparse-checkout'].splitlines()),
                         {'/.github/actions/', '/scripts/ci-base-image.py',
                          '/scripts/ci-base-images.json'})

    def test_pr_gate_runs_complete_image_contract_without_registry_write_token(self):
        root = Path(__file__).resolve().parent.parent
        workflow = yaml.load((root / '.github/workflows/pr-gate.yml').read_text(),
                             Loader=yaml.BaseLoader)
        self.assertIn('pull_request', workflow['on'])
        self.assertNotIn('packages', workflow['permissions'])
        job = workflow['jobs']['ci-base-image-policy']
        self.assertNotIn('if', job)
        self.assertNotIn('needs', job)
        self.assertEqual(job['permissions'], {'contents': 'read'})
        self.assertEqual(job['steps'][0]['with']['persist-credentials'], 'false')
        commands = [step.get('run', '') for step in job['steps']]
        self.assertIn('bash scripts/ci-install-ubuntu-packages.sh skopeo docker-registry python3-yaml',
                      commands)
        self.assertIn('python3 scripts/test-ci-base-image.py', commands)
        self.assertLess(commands.index('bash scripts/ci-install-ubuntu-packages.sh skopeo docker-registry python3-yaml'),
                        commands.index('python3 scripts/test-ci-base-image.py'))
        shards = workflow['jobs']['workspace-test-shards']
        self.assertIn('ci-base-image-policy', shards['needs'])
        self.assertEqual(shards['if'], '${{ always() }}')
        gate = next(step for step in shards['steps']
                    if step.get('name') == 'Require CI base image policy')
        self.assertEqual(gate['env']['IMAGE_POLICY_RESULT'],
                         '${{ needs.ci-base-image-policy.result }}')
        self.assertEqual(gate['run'], 'test "$IMAGE_POLICY_RESULT" = success')
        required = workflow['jobs']['workspace-tests']
        self.assertEqual(required['name'], 'workspace-tests')
        self.assertEqual(required['needs'], 'workspace-test-shards')
        self.assertEqual(required['if'], '${{ always() }}')
        gate = next(step for step in required['steps']
                    if step.get('name') == 'Require every workspace test shard')
        self.assertEqual(gate['env']['SHARDS_RESULT'],
                         '${{ needs.workspace-test-shards.result }}')
        self.assertEqual(gate['run'], 'test "$SHARDS_RESULT" = success')
        lifecycle = workflow['jobs']['native-cross-source-lifecycle']['steps']
        install = next(step for step in lifecycle
                       if step.get('name') == 'Install release-backed base image transport')
        action = next(step for step in lifecycle
                      if step.get('uses') == './.github/actions/cache-base-image')
        build = next(step for step in lifecycle
                     if step.get('name') == 'Build the ${{ matrix.distro }} test image')
        cleanup = next(step for step in lifecycle
                       if step.get('name') == 'Stop the release-backed local registry')
        self.assertEqual(install['if'], "${{ matrix.distro == 'opensuse-tumbleweed' }}")
        self.assertEqual(install['run'],
                         'bash scripts/ci-install-ubuntu-packages.sh skopeo docker-registry')
        self.assertLess(lifecycle.index(install), lifecycle.index(action))
        self.assertLess(lifecycle.index(action), lifecycle.index(build))
        self.assertLess(lifecycle.index(build), lifecycle.index(cleanup))
        self.assertEqual(cleanup['if'], "${{ always() && matrix.distro == 'opensuse-tumbleweed' }}")


if __name__ == '__main__':
    unittest.main()
