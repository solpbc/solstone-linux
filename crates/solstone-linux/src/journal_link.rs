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
    if scheme.eq_ignore_ascii_case("solstone") {
        if rest
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
        return if address_ok && valid_token(token) {
            Incoming::Open(format!("/app/open/{token}"))
        } else {
            Incoming::Invalid
        };
    }
    if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http") {
        return classify_web(scheme, rest);
    }
    Incoming::Other
}

fn classify_web(scheme: &str, rest: &str) -> Incoming {
    let Some(without_slashes) = rest.strip_prefix("//") else {
        return Incoming::Other;
    };
    let (before_fragment, token) = match without_slashes.split_once('#') {
        Some((before, fragment)) => (before, Some(fragment)),
        None => (without_slashes, None),
    };
    let (before_query, query) = match before_fragment.split_once('?') {
        Some((before, q)) => (before, Some(q)),
        None => (before_fragment, None),
    };
    let Some((authority, path)) = before_query.split_once('/') else {
        return Incoming::Other;
    };
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host_and_port)| host_and_port);
    let host = host.split_once(':').map_or(host, |(h, _)| h);
    if !host.eq_ignore_ascii_case("go.solstone.app") || !(path == "j" || path.starts_with("j/")) {
        return Incoming::Other;
    }
    // A query on the web link carries nothing of ours (chat apps append their own),
    // so it is ignored as long as it contains no control or whitespace bytes.
    let query_ok = query.is_none_or(|q| {
        !q.bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
    });
    if scheme.eq_ignore_ascii_case("https")
        && authority.eq_ignore_ascii_case("go.solstone.app")
        && path == "j"
        && query_ok
        && let Some(token) = token
        && valid_token(token)
    {
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
            let token = row["t"].as_str().unwrap();
            let expected = row["app_grammar"].as_bool().unwrap();
            for url in [
                format!("solstone://open#{token}"),
                format!("https://go.solstone.app/j#{token}"),
                format!("https://go.solstone.app/j?utm_source=chatgpt.com#{token}"),
            ] {
                assert_eq!(
                    matches!(classify(&url), Incoming::Open(_)),
                    expected,
                    "{} ({url})",
                    row["id"]
                );
            }
        }
    }

    #[test]
    fn journal_links_keep_the_exact_opaque_target_and_reject_other_url_parts() {
        for prefix in [
            "solstone://open#",
            "solstone://open/#",
            "SOLSTONE://OPEN/#",
            "https://go.solstone.app/j#",
            "HTTPS://GO.SOLSTONE.APP/j#",
            "https://go.solstone.app/j?#",
            "https://go.solstone.app/j?utm_source=chatgpt.com#",
            "https://go.solstone.app/j?a=1&b=2/3@x:4#",
        ] {
            for token in ["Ab_-9", "Ab_-9~090020", "A~256199"] {
                let Incoming::Open(path) = classify(&format!("{prefix}{token}")) else {
                    panic!("valid link rejected: {prefix}{token}");
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
            "abc?x=1",
            "abc~0900",
            "abc~090020~000000",
            "abc~１２３４５６",
            &"A".repeat(513),
        ] {
            for prefix in [
                "solstone://open#",
                "solstone://open/#",
                "https://go.solstone.app/j#",
                "https://go.solstone.app/j?utm_source=chatgpt.com#",
            ] {
                assert!(
                    matches!(classify(&format!("{prefix}{token}")), Incoming::Invalid),
                    "expected invalid: {prefix}{token}"
                );
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
            "https://go.solstone.app/j",
            "https://go.solstone.app/j?utm_source=chatgpt.com",
            "https://go.solstone.app/j/#abc",
            "https://go.solstone.app/j/x#abc",
            "https://go.solstone.app/j/../j#abc",
            "https://go.solstone.app:443/j#abc",
            "https://go.solstone.app:/j#abc",
            "https://user@go.solstone.app/j#abc",
            "https://user:pass@go.solstone.app/j#abc",
            "https://@go.solstone.app/j#abc",
            "https://go.solstone.app/j?bad space#abc",
            "https://go.solstone.app/j?bad\nline#abc",
            "http://go.solstone.app/j#abc",
        ] {
            assert!(
                matches!(classify(link), Incoming::Invalid),
                "expected invalid: {link}"
            );
        }
        for other in [
            "solstone://pair#abc",
            "https://go.solstone.app/p#abc",
            "https://go.solstone.app/p/#abc",
            "https://go.solstone.app/J#abc",
            "https://go.solstone.app/journal#abc",
            "https://go.solstone.app#abc",
            "https://evil.solstone.app/j#abc",
            "https://go.solstone.app.evil/j#abc",
            "https://go.solstone.app@evil.example/j#abc",
            "https://go.solstone.app:443@evil.example/j#abc",
            "https://go.solstone.app./j#abc",
        ] {
            assert!(
                matches!(classify(other), Incoming::Other),
                "expected other: {other}"
            );
        }
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
