//! The error boundary. `brewcore` returns `thiserror` enums, which are not
//! `Serialize`; commands must return something the frontend can read. Converting
//! here — rather than letting commands return strings — keeps `tauri::Error` out of
//! core and gives the UI a stable `code` to branch on.

#[derive(Debug, serde::Serialize)]
pub struct ApiError {
    /// Stable identifier. Messages change; this must not.
    pub code: String,
    /// For humans, on screen.
    pub message: String,
    /// For the log. May be verbose.
    pub detail: Option<String>,
}

impl From<brewcore::Error> for ApiError {
    fn from(err: brewcore::Error) -> Self {
        Self {
            code: err.code().to_owned(),
            message: err.to_string(),
            detail: Some(format!("{err:?}")),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, path::Path};

    use super::*;

    #[test]
    fn a_core_error_crosses_as_code_message_and_detail() {
        let err = brewcore::Error::BatchNotPitched { batch_id: 7 };
        let message = err.to_string();

        let json = serde_json::to_value(ApiError::from(err)).unwrap();
        let keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["code", "detail", "message"]);
        assert_eq!(json["code"], "db.batch_not_pitched");
        assert_eq!(json["message"], message);
        assert!(json["detail"].as_str().unwrap().contains("BatchNotPitched"));
    }

    /// Every string `brewcore::Error::code` can return, read from the source: the
    /// match is exhaustive, so the compiler already guarantees every variant has one.
    fn core_codes() -> Vec<String> {
        // Normalised because a Windows checkout may have CRLF endings.
        let source = include_str!("../../crates/brewcore/src/error.rs").replace("\r\n", "\n");
        let body = source
            .split_once("pub fn code(&self)")
            .expect("Error::code moved")
            .1;
        let body = &body[..body.find("\n    }\n").expect("end of Error::code")];
        body.split("=> \"")
            .skip(1)
            .map(|rest| rest[..rest.find('"').unwrap()].to_owned())
            .collect()
    }

    #[test]
    fn every_error_code_is_distinct_and_well_formed() {
        let codes = core_codes();
        assert!(codes.len() > 50, "parsed only {} codes", codes.len());

        let mut seen = BTreeSet::new();
        for code in &codes {
            assert!(seen.insert(code), "{code} is returned by two variants");
            assert!(
                code.split('.').all(|part| !part.is_empty()
                    && part.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')),
                "{code} is not dotted snake_case"
            );
        }
    }

    /// The UI branches on codes, so a renamed one breaks a hint silently. Any quoted
    /// `namespace.name` in the frontend or this crate, under a namespace `brewcore`
    /// uses, has to be a code that still exists.
    #[test]
    fn every_code_the_app_names_exists_in_core() {
        let codes: BTreeSet<_> = core_codes().into_iter().collect();
        let namespaces: BTreeSet<_> = codes
            .iter()
            .filter_map(|code| code.split_once('.').map(|(ns, _)| ns.to_owned()))
            .collect();

        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut named = Vec::new();
        for dir in [root.join("../src"), root.join("src")] {
            collect_quoted_codes(&dir, &namespaces, &mut named);
        }
        assert!(
            named.iter().any(|(_, code)| code == "brewd.unconfigured"),
            "the scan found nothing; did Sources.svelte move?"
        );
        for (file, code) in named {
            assert!(
                codes.contains(&code),
                "{file} names {code}, which no error returns"
            );
        }
    }

    fn collect_quoted_codes(
        dir: &Path,
        namespaces: &BTreeSet<String>,
        out: &mut Vec<(String, String)>,
    ) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect_quoted_codes(&path, namespaces, out);
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str());
            // `ipc_tests.rs` gets its codes back from real calls, and names `config.toml`.
            let skip = path.ends_with("error.rs") || path.ends_with("ipc_tests.rs");
            if !matches!(ext, Some("ts" | "svelte" | "rs")) || skip {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            // Every segment between two quotes, not every other one: a `'"'` char literal
            // would shift the parity, and a code has no spaces to be confused with code.
            for quoted in text.split('"') {
                let Some((ns, name)) = quoted.split_once('.') else {
                    continue;
                };
                let is_code = namespaces.contains(ns)
                    && !name.is_empty()
                    && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
                if is_code {
                    out.push((path.display().to_string(), quoted.to_owned()));
                }
            }
        }
    }
}
