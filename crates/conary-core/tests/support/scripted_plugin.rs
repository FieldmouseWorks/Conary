// Dependency-free plugin for the runner tests, compiled once by them with
// rustc. It answers describe from `<exe>.describe`, and the Nth work request
// from line N of `<exe>.answers`; it appends every work request, one per
// line, to `<exe>.requests`. It never interprets a request.
use std::io::{Read, Write};
fn main() {
    let exe = std::env::current_exe().unwrap();
    let file = |ext: &str| exe.with_extension(ext);
    let mut request = String::new();
    std::io::stdin().read_to_string(&mut request).unwrap();
    if request.contains(r#""kind":"describe""#) {
        print!("{}", std::fs::read_to_string(file("describe")).unwrap());
        return;
    }
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file("requests"))
        .unwrap();
    log.write_all(request.as_bytes()).unwrap();
    let seen = std::fs::read_to_string(file("requests")).unwrap().lines().count();
    let answers = std::fs::read_to_string(file("answers")).unwrap();
    print!("{}", answers.lines().nth(seen - 1).unwrap_or(""));
}
