// crates/conary-core/src/source_root/tests/name_grammar.rs

use super::super::{SOURCE_ROOT_NAME_MAX_LEN, SourceRootName, SourceRootNameError};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

#[test]
fn catalogued_profiles_and_grammar_edges_are_accepted() {
    let longest = "a".repeat(SOURCE_ROOT_NAME_MAX_LEN);
    for value in [
        "fedora-44",
        "ubuntu-26.04",
        "arch",
        "a",
        "0",
        "a_b",
        "a.",
        longest.as_str(),
    ] {
        let name = SourceRootName::parse(value).unwrap();
        assert_eq!(name.as_str(), value);
        assert_eq!(name.to_string(), value);
        crate::repository::resolution_policy::validate_source_identity(name.as_str(), "test")
            .unwrap();
    }
}

#[test]
fn names_outside_the_grammar_are_typed_errors() {
    let too_long = "a".repeat(SOURCE_ROOT_NAME_MAX_LEN + 1);
    let cases: Vec<(&str, SourceRootNameError)> = vec![
        ("", SourceRootNameError::Empty),
        (
            too_long.as_str(),
            SourceRootNameError::TooLong {
                len: SOURCE_ROOT_NAME_MAX_LEN + 1,
            },
        ),
        (".", SourceRootNameError::InvalidFirstByte { found: '.' }),
        ("..", SourceRootNameError::InvalidFirstByte { found: '.' }),
        (
            ".creating-arch",
            SourceRootNameError::InvalidFirstByte { found: '.' },
        ),
        (
            "-arch",
            SourceRootNameError::InvalidFirstByte { found: '-' },
        ),
        (
            "_arch",
            SourceRootNameError::InvalidFirstByte { found: '_' },
        ),
        ("Arch", SourceRootNameError::InvalidFirstByte { found: 'A' }),
        ("é", SourceRootNameError::InvalidFirstByte { found: 'é' }),
        (
            "arcH",
            SourceRootNameError::InvalidByte {
                index: 3,
                found: 'H',
            },
        ),
        (
            "a/b",
            SourceRootNameError::InvalidByte {
                index: 1,
                found: '/',
            },
        ),
        (
            "a b",
            SourceRootNameError::InvalidByte {
                index: 1,
                found: ' ',
            },
        ),
        (
            "a\0",
            SourceRootNameError::InvalidByte {
                index: 1,
                found: '\0',
            },
        ),
        (
            "a+b",
            SourceRootNameError::InvalidByte {
                index: 1,
                found: '+',
            },
        ),
    ];
    for (value, expected) in cases {
        assert_eq!(
            SourceRootName::parse(value),
            Err(expected),
            "value {value:?}"
        );
    }
}

#[test]
fn directory_names_must_be_utf8() {
    assert_eq!(
        SourceRootName::from_os_str(OsStr::from_bytes(b"arch\xff")),
        Err(SourceRootNameError::NotUtf8)
    );
    assert_eq!(
        SourceRootName::from_os_str(OsStr::new("arch"))
            .unwrap()
            .as_str(),
        "arch"
    );
}
