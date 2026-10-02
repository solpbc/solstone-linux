// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Recognising a browser's native-messaging launch from argv alone, before any other
//! startup work. The id check is a consistency check, not caller authentication: any
//! process of the same user can launch this binary with any arguments.

use native_browser_frame::{DEV_CHROME_ID, DEV_FIREFOX_ID, PROD_CHROME_ID, PROD_FIREFOX_ID};
use std::ffi::OsString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Production,
    Development,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Production => "production",
            Channel::Development => "development",
        }
    }
}

/// Chrome and Edge share one extension id, so a Chromium launch is told apart only by
/// the extension's own `hello`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrandHint {
    Chromium,
    Firefox,
}

impl BrandHint {
    pub fn as_str(self) -> &'static str {
        match self {
            BrandHint::Chromium => "chromium",
            BrandHint::Firefox => "firefox",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostInvocation {
    pub brand: BrandHint,
    pub channel: Channel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recognition {
    /// An ordinary command line; the CLI handles it.
    NotHost,
    Host(HostInvocation),
    /// Looks like a native-messaging launch, but for an extension this build does not
    /// admit.
    Refused,
}

const CHROME_ORIGIN_PREFIX: &str = "chrome-extension://";

/// `args` excludes argv[0]. Chromium passes `chrome-extension://<id>/`; Firefox passes
/// the manifest path and then the extension id.
pub fn recognize(args: &[OsString], dev_enabled: bool) -> Recognition {
    let args: Option<Vec<&str>> = args.iter().map(|arg| arg.to_str()).collect();
    let Some(args) = args else {
        return Recognition::NotHost;
    };
    let channel_for = |production: bool, development: bool| match (production, development) {
        (true, false) => Recognition::Host(HostInvocation {
            brand: BrandHint::Chromium,
            channel: Channel::Production,
        }),
        (false, true) if dev_enabled => Recognition::Host(HostInvocation {
            brand: BrandHint::Chromium,
            channel: Channel::Development,
        }),
        _ => Recognition::Refused,
    };
    match args.as_slice() {
        [origin] if origin.starts_with(CHROME_ORIGIN_PREFIX) => {
            let Some(id) = origin
                .strip_prefix(CHROME_ORIGIN_PREFIX)
                .and_then(|rest| rest.strip_suffix('/'))
            else {
                return Recognition::Refused;
            };
            channel_for(id == PROD_CHROME_ID, id == DEV_CHROME_ID)
        }
        [first, ..] if first.starts_with(CHROME_ORIGIN_PREFIX) => Recognition::Refused,
        [manifest, id] if is_firefox_launch(manifest, id) => {
            if manifest.is_empty() {
                return Recognition::Refused;
            }
            match channel_for(*id == PROD_FIREFOX_ID, *id == DEV_FIREFOX_ID) {
                Recognition::Host(invocation) => Recognition::Host(HostInvocation {
                    brand: BrandHint::Firefox,
                    ..invocation
                }),
                other => other,
            }
        }
        _ => Recognition::NotHost,
    }
}

/// Firefox's launch: a manifest path and an add-on id. Ids are `name@domain` or a
/// braced UUID; the CLI has no two-argument form that looks like this.
fn is_firefox_launch(manifest: &str, id: &str) -> bool {
    !manifest.starts_with('-')
        && manifest.ends_with(".json")
        && (id.contains('@') || (id.starts_with('{') && id.ends_with('}')))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn host(brand: BrandHint, channel: Channel) -> Recognition {
        Recognition::Host(HostInvocation { brand, channel })
    }

    #[test]
    fn production_ids_are_recognised_in_every_build() {
        for dev in [false, true] {
            assert_eq!(
                recognize(
                    &args(&["chrome-extension://eibbeeoifjoabddfmgeggnageolkcnim/"]),
                    dev
                ),
                host(BrandHint::Chromium, Channel::Production)
            );
            assert_eq!(
                recognize(
                    &args(&[
                        "/home/o/.mozilla/native-messaging-hosts/app.solstone.browser.json",
                        "browser@solstone.app"
                    ]),
                    dev
                ),
                host(BrandHint::Firefox, Channel::Production)
            );
        }
    }

    #[test]
    fn development_ids_are_refused_unless_the_build_admits_them() {
        let chromium = args(&["chrome-extension://fgfnkcefedeheoeamppkiiloncfekakf/"]);
        let firefox = args(&[
            "/m/app.solstone.browser.dev.json",
            "browser.dev@solstone.app",
        ]);
        assert_eq!(recognize(&chromium, false), Recognition::Refused);
        assert_eq!(recognize(&firefox, false), Recognition::Refused);
        assert_eq!(
            recognize(&chromium, true),
            host(BrandHint::Chromium, Channel::Development)
        );
        assert_eq!(
            recognize(&firefox, true),
            host(BrandHint::Firefox, Channel::Development)
        );
    }

    #[test]
    fn native_looking_launches_for_other_extensions_are_refused() {
        for argv in [
            vec!["chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/"],
            vec!["chrome-extension://eibbeeoifjoabddfmgeggnageolkcnim"],
            vec![
                "chrome-extension://eibbeeoifjoabddfmgeggnageolkcnim/",
                "--parent-window=0",
            ],
            vec!["/m/app.solstone.browser.json", "other@example.org"],
            vec![
                "/m/app.solstone.browser.json",
                "{9f1c2c9e-0000-0000-0000-000000000000}",
            ],
        ] {
            assert_eq!(
                recognize(&args(&argv), true),
                Recognition::Refused,
                "{argv:?}"
            );
        }
    }

    #[test]
    fn ordinary_command_lines_are_left_to_the_cli() {
        for argv in [
            vec![],
            vec!["run"],
            vec!["status"],
            vec!["setup", "--mark"],
            vec!["pause", "--minutes"],
            vec!["--help"],
        ] {
            assert_eq!(
                recognize(&args(&argv), true),
                Recognition::NotHost,
                "{argv:?}"
            );
        }
    }
}
