#!/usr/bin/env python3
"""Image retention contract, including real disposable Distribution registries."""
import contextlib
import gzip
import hashlib
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
import time
import tomllib
import unittest
import urllib.error
import urllib.request

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
    digest = hashlib.sha256(manifest).hexdigest()
    return {'source': f'origin.example/source:reviewed@sha256:{digest}',
            'retained': f'retained.example/image:reviewed@sha256:{digest}', 'platform': 'linux/amd64'}


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
        self.assertIn('@sha256:', image.resolve(self.entry['retained'], entries))
        with self.assertRaisesRegex(ValueError, 'retained reference'):
            image.resolve(self.entry['source'], entries)
        with self.assertRaisesRegex(ValueError, 'retained reference'):
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

    def test_corrupt_manifest_config_and_layer_fail_independently(self):
        for path in self.layout.iterdir():
            with self.subTest(name=path.name):
                original = path.read_bytes()
                path.write_bytes(bytes([original[0] ^ 1]) + original[1:])
                with self.assertRaisesRegex(ValueError, 'digest mismatch'):
                    image.verify(self.layout, self.entry)
                path.write_bytes(original)

    def test_missing_truncated_symlinked_and_wrong_authority_fail(self):
        path = next(p for p in self.layout.iterdir() if p.name != 'manifest.json')
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
        for change in ({'retained': 'retained.example/image@sha256:' + '0' * 64},
                       {'platform': 'linux/arm64'}, {'unexpected': 'value'}):
            path.write_text(json.dumps({'version': 1, 'images': {'fixture': {**self.entry, **change}}}))
            with self.assertRaises(ValueError):
                image.catalog(path)

    def test_shipped_retained_reference_matches_typed_image_configuration(self):
        root = Path(__file__).resolve().parent.parent
        config = tomllib.loads((root / 'apps/conary/tests/integration/remi/config.toml').read_text())
        for name, entry in image.catalog(image.CATALOG).items():
            self.assertEqual(config['distros'][name]['target_root']['base_image'], entry['retained'])

    def test_cold_retained_pull_survives_deleted_origin_manifest(self):
        # No host image daemon/store, package install or image execution.
        auth = self.root / 'anonymous.json'
        auth.write_text('{"auths":{}}')
        with registry(self.root / 'origin-registry') as origin, registry(self.root / 'retained-registry') as retained:
            _, digest = image.reference(self.entry['source'])
            entry = {'source': f'{origin}/source@sha256:{digest}',
                     'retained': f'{retained}/retained@sha256:{digest}', 'platform': 'linux/amd64'}
            image.copy_image(f'dir:{self.layout}', f'docker://{origin}/source:reviewed',
                             ['--dest-authfile', str(auth), '--dest-tls-verify=false'])
            staged = self.root / 'staged'
            image.fetch(entry, staged, origin=True, allow_http=True)
            self.assertTrue(image.publish(entry, staged, auth, allow_http=True)['anonymous_read_verified'])
            url = f'http://{origin}/v2/source/manifests/sha256:{digest}'
            manifest_request = urllib.request.Request(
                url, headers={'Accept': 'application/vnd.oci.image.manifest.v1+json'})
            with urllib.request.urlopen(manifest_request, timeout=5) as response:
                self.assertEqual(response.status, 200)
            with urllib.request.urlopen(urllib.request.Request(url, method='DELETE'), timeout=5) as response:
                self.assertEqual(response.status, 202)
            with self.assertRaises(urllib.error.HTTPError) as failure:
                urllib.request.urlopen(manifest_request, timeout=5)
            self.assertEqual(failure.exception.code, 404)
            failure.exception.close()
            shutil.rmtree(staged)
            shutil.rmtree(self.layout)
            receipt = image.fetch(entry, self.root / 'cold-read', allow_http=True)
            self.assertEqual(receipt['manifest_sha256'], digest)
            self.assertEqual(len(receipt['blobs']), 2)
            self.assertFalse(staged.exists())
            self.assertFalse(self.layout.exists())
            # A healthy origin must not become an implicit fallback when the
            # retained authority is unavailable.
            image.copy_image(f'dir:{self.root / "cold-read"}', f'docker://{origin}/source:reviewed',
                             ['--dest-authfile', str(auth), '--dest-tls-verify=false'])
            with urllib.request.urlopen(manifest_request, timeout=5) as response:
                self.assertEqual(response.status, 200)
            retained_url = f'http://{retained}/v2/retained/manifests/sha256:{digest}'
            with urllib.request.urlopen(urllib.request.Request(retained_url, method='DELETE'), timeout=5):
                pass
            with self.assertRaises(subprocess.CalledProcessError):
                image.fetch(entry, self.root / 'missing-retained', allow_http=True)


if __name__ == '__main__':
    unittest.main()
