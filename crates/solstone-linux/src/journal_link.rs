// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! An opaque journal destination. Never decode it or include it in diagnostics.

pub enum Incoming {
    Other,
    Invalid,
    Open(String),
}

pub fn classify(url: &str) -> Incoming {
    let Some((scheme, rest)) = url.split_once(':') else {
        return Incoming::Other;
    };
    if !scheme.eq_ignore_ascii_case("solstone")
        || rest
            .get(..6)
            .is_some_and(|host| host.eq_ignore_ascii_case("//pair"))
    {
        return Incoming::Other;
    }
    let Some((authority, token)) = rest.split_once('#') else {
        return Incoming::Invalid;
    };
    // Some browsers hand the link over with an empty root path: `solstone://open/#…`.
    let address_ok = ["//open", "//open/"]
        .iter()
        .any(|address| authority.eq_ignore_ascii_case(address));
    if address_ok && valid_token(token) {
        Incoming::Open(format!("/app/open/{token}"))
    } else {
        Incoming::Invalid
    }
}

pub fn valid_path(path: &str) -> bool {
    path.strip_prefix("/app/open/").is_some_and(valid_token)
}

fn valid_token(token: &str) -> bool {
    let (sealed, moment) = token
        .split_once('~')
        .map_or((token, None), |(s, m)| (s, Some(m)));
    !sealed.is_empty()
        && sealed.len() <= 512
        && sealed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        && moment.is_none_or(|m| m.len() == 6 && m.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_journal_fixture_vector_has_the_same_grammar_verdicts() {
        let vector: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/journal-link-vector.json"
        ))
        .unwrap();
        for row in vector["links"].as_array().unwrap() {
            let url = format!("solstone://open#{}", row["t"].as_str().unwrap());
            assert_eq!(
                matches!(classify(&url), Incoming::Open(_)),
                row["app_grammar"].as_bool().unwrap(),
                "{}",
                row["id"]
            );
        }
    }

    #[test]
    fn journal_links_keep_the_exact_opaque_target_and_reject_other_url_parts() {
        for prefix in ["solstone://open#", "solstone://open/#", "SOLSTONE://OPEN/#"] {
            for token in ["Ab_-9", "Ab_-9~090020", "A~256199"] {
                let Incoming::Open(path) = classify(&format!("{prefix}{token}")) else {
                    panic!("valid link rejected");
                };
                assert_eq!(path, format!("/app/open/{token}"));
                assert!(valid_path(&path));
            }
        }
        assert!(valid_token(&"A".repeat(512)));
        for token in [
            "",
            "abc=",
            "abc%2F",
            "abc/",
            "abc#def",
            "abc~0900",
            "abc~090020~000000",
            "abc~１２３４５６",
            &"A".repeat(513),
        ] {
            for prefix in ["solstone://open#", "solstone://open/#"] {
                assert!(matches!(
                    classify(&format!("{prefix}{token}")),
                    Incoming::Invalid
                ));
            }
        }
        for link in [
            "solstone://open",
            "solstone://open//#abc",
            "solstone://open/x#abc",
            "solstone://open?x#abc",
            "solstone://open/?x#abc",
            "solstone://open.evil#abc",
            "solstone://open:99#abc",
            "solstone://user@open#abc",
            "solstone://open/../#abc",
        ] {
            assert!(matches!(classify(link), Incoming::Invalid));
        }
        assert!(matches!(classify("solstone://pair#abc"), Incoming::Other));
        for path in [
            "/",
            "/app/open/abc?x",
            "//other/app/open/abc",
            "/app/open/abc/../",
        ] {
            assert!(!valid_path(path));
        }
    }
}
