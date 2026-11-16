//! Extract a browser session from a DevTools "Copy as cURL" of any Notion /api/v3/ request.

use crate::error::{AppError, Result};
use regex::Regex;

#[derive(Debug, PartialEq)]
pub struct BrowserSession {
    pub base_url: String,
    /// Kept exactly as the browser sends it (often percent-encoded, e.g. `v03%3A...`).
    pub token_v2: String,
    pub user_id: String,
    pub space_id: Option<String>,
    pub user_agent: Option<String>,
}

fn capture(re: &str, text: &str) -> Option<String> {
    Regex::new(re)
        .unwrap()
        .captures(text)
        .and_then(|c| {
            c.iter()
                .skip(1)
                .flatten()
                .next()
                .map(|m| m.as_str().trim().to_string())
        })
        .filter(|s| !s.is_empty())
}

pub fn parse(curl: &str) -> Result<BrowserSession> {
    let host = capture(r"https://((?:[\w-]+\.)*notion\.(?:so|com))/", curl)
        .ok_or_else(|| AppError::input("no https://www.notion.so URL found in the cURL"))?;
    let token_v2 = capture(r#"(?:^|[\s;'"])token_v2=([^;'"\s]+)"#, curl)
        .ok_or_else(|| AppError::input("no token_v2 cookie found in the cURL"))?;
    let uuid = r"([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})";
    let user_id = capture(&format!(r"(?i)x-notion-active-user-header:\s*{uuid}"), curl)
        .or_else(|| capture(&format!(r"(?:^|[\s;'\x22])notion_user_id={uuid}"), curl))
        .ok_or_else(|| {
            AppError::input("no x-notion-active-user-header or notion_user_id cookie in the cURL")
        })?;
    let space_id = capture(&format!(r"(?i)x-notion-space-id:\s*{uuid}"), curl);
    let user_agent = capture(
        r#"(?:-H|--header)\s+['"][Uu]ser-[Aa]gent:\s*([^'"]+)['"]|(?:^|\s)(?:-A|--user-agent)\s+['"]([^'"]+)['"]"#,
        curl,
    )
    .filter(|ua| !ua.chars().any(char::is_control));
    Ok(BrowserSession {
        base_url: format!("https://{host}"),
        token_v2,
        user_id,
        space_id,
        user_agent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const U: &str = "0a1b2c3d-1111-2222-3333-444455556666";
    const S: &str = "9f8e7d6c-aaaa-bbbb-cccc-ddddeeeeffff";

    #[test]
    fn parses_chrome_copy_as_curl() {
        let curl = format!(
            r#"curl 'https://www.notion.so/api/v3/loadUserContent' \
  -H 'accept: */*' \
  -b 'notion_browser_id=x; token_v2=v03%3AeyJhbGc.abc; notion_user_id={U}; csrf=1' \
  -H 'notion-client-version: 23.13' \
  -H 'user-agent: Mozilla/5.0 (Macintosh) Chrome/140' \
  -H 'x-notion-active-user-header: {U}' \
  -H 'x-notion-space-id: {S}' \
  --data-raw '{{}}'"#
        );
        let s = parse(&curl).unwrap();
        assert_eq!(s.base_url, "https://www.notion.so");
        assert_eq!(s.token_v2, "v03%3AeyJhbGc.abc");
        assert_eq!(s.user_id, U);
        assert_eq!(s.space_id.as_deref(), Some(S));
        assert_eq!(
            s.user_agent.as_deref(),
            Some("Mozilla/5.0 (Macintosh) Chrome/140")
        );
    }

    #[test]
    fn cookie_header_form_and_missing_parts() {
        let curl = format!(
            "curl 'https://www.notion.so/api/v3/search' -H 'Cookie: token_v2=abc; notion_user_id={U}'"
        );
        let s = parse(&curl).unwrap();
        assert_eq!((s.token_v2.as_str(), s.user_id.as_str()), ("abc", U));
        assert!(s.space_id.is_none());
        assert!(parse("curl 'https://www.notion.so/api/v3/x' -b 'notion_user_id=1'").is_err());
        assert!(parse("curl 'https://evil.com/api' -b 'token_v2=a'").is_err());
    }
}
