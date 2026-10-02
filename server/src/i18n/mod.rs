//! The languages ThirtyFile speaks: English (the source text, and the fallback), Traditional Chinese (`zh-TW`),
//! Simplified Chinese (`zh-CN`) and Japanese (`ja`), and which one a request or an email gets.
//!
//! - The interface itself is translated in the browser (web/src/lib/i18n/); the server decides which dictionary the
//!   page carries (web.rs) and tells the page the language the person or the system default picked
//! - A request's language ([`resolve`]): for someone signed in, the language saved with their account, then the one
//!   chosen in this browser (the `tf_lang` cookie, with `tf_lang_chosen`), then the system default (`default_lang`),
//!   then the browser's languages (`Accept-Language`), then English. A visitor who isn't signed in (the sign-in page,
//!   share links, resetting a password): the language chosen in this browser, then the browser's, then the system
//!   default, then English. People from outside who open a share link read their own language
//! - An email's language ([`recipient`]): the recipient's saved language, else the system default, else the language
//!   they last used, else English. Never that of the person whose action caused it
//! - The texts the server writes itself (the emails) are in texts.rs, in English, with one file of translations per
//!   language (zh_tw.rs, zh_cn.rs, ja.rs); [`tr`] looks a text up, falling back to English

mod ja;
mod texts;
mod zh_cn;
mod zh_tw;

pub use texts::{Text, tr};

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, header},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    auth::{User, get_cookie},
    error::{AppError, AppResult},
    state::AppState,
    util::{now, sha256_hex},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    En,
    ZhTw,
    ZhCn,
    Ja,
}

/// A language's names and whether the interface offers it
struct Info {
    lang: Lang,
    /// As the interface, the cookies, the settings and the database name it
    code: &'static str,
    /// `<html lang>`: Chinese and Japanese share characters drawn differently, so the tag carries the script
    html: &'static str,
    /// Offered in the language switch and picked from the browser's languages: the same as `ready` in
    /// web/src/lib/i18n.ts (a test keeps them alike). A language that isn't translated yet stays hidden
    offered: bool,
}

const LANGUAGES: [Info; 4] = [
    Info { lang: Lang::En, code: "en", html: "en", offered: true },
    Info { lang: Lang::ZhTw, code: "zh-TW", html: "zh-Hant", offered: true },
    Info { lang: Lang::ZhCn, code: "zh-CN", html: "zh-Hans", offered: false },
    Info { lang: Lang::Ja, code: "ja", html: "ja", offered: false },
];

impl Lang {
    /// Every language, English first
    pub const ALL: [Lang; 4] = [Lang::En, Lang::ZhTw, Lang::ZhCn, Lang::Ja];

    fn info(self) -> &'static Info {
        LANGUAGES.iter().find(|i| i.lang == self).expect("every language is listed")
    }

    pub fn code(self) -> &'static str {
        self.info().code
    }

    pub fn html_tag(self) -> &'static str {
        self.info().html
    }

    pub fn offered(self) -> bool {
        self.info().offered
    }

    /// A language by its code exactly as ThirtyFile writes it (`zh-TW`); None for anything else, "auto" and ""
    pub fn parse(code: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|l| l.code() == code)
    }

    /// The language for a browser's language tag, matched leniently like the page does (`matchLanguage`):
    /// `zh-Hant`, `zh-TW`, `zh-HK` and `zh-MO` → zh-TW; `zh-Hans`, `zh-CN`, `zh-SG` and plain `zh` → zh-CN; `ja…` → ja;
    /// `en…` → en. A script subtag decides before a region (`zh-Hans-HK` is Simplified)
    pub fn from_tag(tag: &str) -> Option<Lang> {
        let tag = tag.trim().to_ascii_lowercase();
        let mut parts = tag.split(['-', '_']);
        let rest: Vec<&str> = match parts.next()? {
            "en" => return Some(Lang::En),
            "ja" => return Some(Lang::Ja),
            "zh" => parts.collect(),
            _ => return None,
        };
        if rest.contains(&"hant") {
            return Some(Lang::ZhTw);
        }
        if rest.contains(&"hans") {
            return Some(Lang::ZhCn);
        }
        Some(if rest.iter().any(|r| matches!(*r, "tw" | "hk" | "mo")) { Lang::ZhTw } else { Lang::ZhCn })
    }
}

/// The language for the browser's languages (`Accept-Language`), in their order of preference: the first one that is
/// offered. Simplified Chinese falls back to Traditional Chinese while it isn't offered (a Chinese reader rather reads
/// that than English); another language that isn't offered gives way to the browser's next one. The same as the page's
/// `browserLanguage`. None when nothing matches
pub fn browser_language(headers: &HeaderMap) -> Option<Lang> {
    let value = headers.get(header::ACCEPT_LANGUAGE)?.to_str().ok()?;
    let mut tags: Vec<(&str, f32)> = value
        .split(',')
        .filter_map(|item| {
            let mut parts = item.split(';');
            let tag = parts.next()?.trim();
            let q = parts.find_map(|p| p.trim().strip_prefix("q=")).map_or(Some(1.0), |q| q.trim().parse::<f32>().ok())?;
            (!tag.is_empty() && q > 0.0).then_some((tag, q))
        })
        .take(32)
        .collect();
    // A stable sort keeps the order of tags with the same weight
    tags.sort_by(|a, b| b.1.total_cmp(&a.1));
    tags.into_iter().find_map(|(tag, _)| match Lang::from_tag(tag)? {
        l if l.offered() => Some(l),
        Lang::ZhCn if Lang::ZhTw.offered() => Some(Lang::ZhTw),
        _ => None,
    })
}

/// The language chosen in this browser: `tf_lang`, when `tf_lang_chosen` says the person picked it. Without that the
/// page only mirrors the language it showed into `tf_lang` (notify.rs keeps it as the language last used)
fn chosen_here(headers: &HeaderMap) -> Option<Lang> {
    get_cookie(headers, "tf_lang_chosen").and(get_cookie(headers, "tf_lang")).and_then(Lang::parse)
}

/// The system default language; None when it follows the browser ("auto")
pub fn system_default(st: &AppState) -> Option<Lang> {
    Lang::parse(&st.system.read().unwrap().default_lang)
}

/// Who a request comes from, as far as its language goes
#[derive(Debug, Clone, Copy)]
pub enum Visitor {
    /// Signed in, with the language saved with their account (if any)
    SignedIn(Option<Lang>),
    /// Not signed in
    Anonymous,
}

/// The language the person or the administrators picked, before the browser's languages count: for someone signed in
/// their saved language, the one chosen in this browser, or the system default; for a visitor who isn't, only the one
/// chosen in this browser. None leaves it to the browser
pub fn picked(headers: &HeaderMap, visitor: Visitor, default: Option<Lang>) -> Option<Lang> {
    match visitor {
        Visitor::SignedIn(saved) => saved.or_else(|| chosen_here(headers)).or(default),
        Visitor::Anonymous => chosen_here(headers),
    }
}

/// The language of a request (see the module's description)
pub fn resolve(headers: &HeaderMap, visitor: Visitor, default: Option<Lang>) -> Lang {
    picked(headers, visitor, default).or_else(|| browser_language(headers)).or(default).unwrap_or(Lang::En)
}

/// The language of an email to someone: their saved language (`users.chosen_lang`), else the system default, else the
/// language they last used (`users.lang`), else English
pub fn recipient(chosen: &str, last_used: &str, default: Option<Lang>) -> Lang {
    Lang::parse(chosen).or(default).or_else(|| Lang::parse(last_used)).unwrap_or(Lang::En)
}

/// Who a page request comes from, by its sign-in cookie (pages don't go through the `User` extractor)
pub async fn visitor(st: &AppState, headers: &HeaderMap) -> Visitor {
    let Some(token) = get_cookie(headers, crate::auth::SESSION_COOKIE) else { return Visitor::Anonymous };
    let found: Result<Option<(String,)>, _> =
        sqlx::query_as("SELECT u.chosen_lang FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token_hash = ? AND s.expires_at > ? AND u.disabled = 0")
            .bind(sha256_hex(token.as_bytes()))
            .bind(now())
            .fetch_optional(&st.db)
            .await;
    match found {
        Ok(Some((chosen,))) => Visitor::SignedIn(Lang::parse(&chosen)),
        _ => Visitor::Anonymous,
    }
}

/// The language saved with an account, if any
pub async fn saved(st: &AppState, user_id: i64) -> AppResult<Option<Lang>> {
    let (chosen,): (String,) = sqlx::query_as("SELECT chosen_lang FROM users WHERE id = ?").bind(user_id).fetch_one(&st.db).await?;
    Ok(Lang::parse(&chosen))
}

/// What /api/auth/me says about the person's language
#[derive(Debug, Serialize)]
pub struct MeLang {
    /// The language saved with the account; "" when none was chosen
    pub lang: &'static str,
    /// The language pages use for them in this browser when they or the system default picked it ([`picked`]); null
    /// when the browser's languages decide. The page switches to it after signing in
    pub ui_lang: Option<&'static str>,
}

pub async fn me_lang(st: &AppState, user_id: i64, headers: &HeaderMap) -> AppResult<MeLang> {
    let saved = saved(st, user_id).await?;
    let ui = picked(headers, Visitor::SignedIn(saved), system_default(st));
    Ok(MeLang { lang: saved.map_or("", Lang::code), ui_lang: ui.map(Lang::code) })
}

#[derive(Deserialize)]
pub struct LanguageReq {
    /// A language's code, or "" to forget the choice
    lang: String,
}

/// Saves the language the person chose in the interface with their account, so it follows them to other devices
pub async fn save(State(st): State<AppState>, user: User, Json(req): Json<LanguageReq>) -> AppResult<Json<Value>> {
    let lang = match req.lang.as_str() {
        "" => None,
        code => Some(Lang::parse(code).ok_or_else(|| AppError::bad_request("Unknown language"))?),
    };
    let code = lang.map_or("", Lang::code);
    let _w = st.write_lock.lock().await;
    // It is also the language they use from now on, for emails
    sqlx::query("UPDATE users SET chosen_lang = ?1, lang = CASE WHEN ?1 = '' THEN lang ELSE ?1 END WHERE id = ?2").bind(code).bind(user.id).execute(&st.db).await?;
    Ok(Json(json!({ "lang": code })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(k.parse::<header::HeaderName>().unwrap(), v.parse().unwrap());
        }
        h
    }

    #[test]
    fn browser_language_tags_match_the_four_languages_leniently() {
        for (tag, want) in [
            ("en", Some(Lang::En)),
            ("en-GB", Some(Lang::En)),
            ("zh-TW", Some(Lang::ZhTw)),
            ("zh-Hant", Some(Lang::ZhTw)),
            ("zh-HK", Some(Lang::ZhTw)),
            ("zh_MO", Some(Lang::ZhTw)),
            ("zh-Hant-CN", Some(Lang::ZhTw)),
            ("zh", Some(Lang::ZhCn)),
            ("zh-CN", Some(Lang::ZhCn)),
            ("zh-Hans", Some(Lang::ZhCn)),
            ("zh-SG", Some(Lang::ZhCn)),
            ("zh-Hans-HK", Some(Lang::ZhCn)),
            ("ja", Some(Lang::Ja)),
            ("JA-jp", Some(Lang::Ja)),
            ("fr", None),
            ("", None),
            ("*", None),
        ] {
            assert_eq!(Lang::from_tag(tag), want, "{tag}");
        }
        for l in Lang::ALL {
            assert_eq!(Lang::parse(l.code()), Some(l));
            assert_eq!(Lang::from_tag(l.code()), Some(l));
        }
        assert_eq!(Lang::parse("auto"), None);
        assert_eq!(Lang::parse("zh-tw"), None, "codes are exact; tags are matched leniently");
    }

    #[test]
    fn the_browsers_preferred_language_that_is_offered_wins() {
        let accept = |v: &str| browser_language(&headers(&[("accept-language", v)]));
        assert_eq!(accept("zh-TW,zh;q=0.9,en;q=0.8"), Some(Lang::ZhTw));
        assert_eq!(accept("fr-FR, en-US;q=0.5"), Some(Lang::En));
        // By weight, not by place; equal weights keep their order
        assert_eq!(accept("en;q=0.5, zh-TW;q=0.8"), Some(Lang::ZhTw));
        assert_eq!(accept("en, zh-TW"), Some(Lang::En));
        assert_eq!(accept("zh-TW;q=0, en;q=0.1"), Some(Lang::En), "q=0 means not wanted");
        assert_eq!(accept("fr, de"), None);
        assert_eq!(accept("*"), None);
        assert_eq!(accept(""), None);
        assert_eq!(browser_language(&HeaderMap::new()), None);
        // Languages that aren't offered yet: Simplified Chinese reads Traditional Chinese, Japanese the next language
        if !Lang::ZhCn.offered() {
            assert_eq!(accept("zh-CN,zh;q=0.9"), Some(Lang::ZhTw));
        }
        if !Lang::Ja.offered() {
            assert_eq!(accept("ja-JP"), None);
            assert_eq!(accept("ja-JP, en;q=0.5"), Some(Lang::En));
        }
    }

    #[test]
    fn signed_in_the_saved_language_comes_first_then_the_browsers_choice_then_the_default() {
        let chosen = |l: &str| format!("tf_lang={l}; tf_lang_chosen=1");
        let english = headers(&[("accept-language", "en-US")]);
        let chinese = headers(&[("accept-language", "zh-TW")]);
        let picked_ja = headers(&[("accept-language", "zh-TW"), ("cookie", &chosen("ja"))]);
        let mirrored = headers(&[("accept-language", "en-US"), ("cookie", "tf_lang=zh-TW")]);
        let me = |saved| Visitor::SignedIn(saved);

        assert_eq!(resolve(&picked_ja, me(Some(Lang::ZhCn)), Some(Lang::En)), Lang::ZhCn, "saved with the account");
        assert_eq!(resolve(&picked_ja, me(None), Some(Lang::En)), Lang::Ja, "chosen in this browser");
        assert_eq!(resolve(&chinese, me(None), Some(Lang::En)), Lang::En, "the system default before the browser");
        assert_eq!(resolve(&chinese, me(None), None), Lang::ZhTw, "the browser");
        assert_eq!(resolve(&HeaderMap::new(), me(None), None), Lang::En);
        // tf_lang without tf_lang_chosen only mirrors what the page showed: it isn't a choice
        assert_eq!(resolve(&mirrored, me(None), None), Lang::En);
        assert_eq!(resolve(&mirrored, me(None), Some(Lang::ZhCn)), Lang::ZhCn);
        // What the page is told: everything but the browser
        assert_eq!(picked(&english, me(None), None), None);
        assert_eq!(picked(&english, me(None), Some(Lang::ZhTw)), Some(Lang::ZhTw));
        assert_eq!(picked(&picked_ja, me(Some(Lang::En)), None), Some(Lang::En));
    }

    #[test]
    fn visitors_who_arent_signed_in_get_their_choice_then_their_browsers_language_then_the_default() {
        let chosen = headers(&[("accept-language", "en"), ("cookie", "tf_lang_chosen=1; tf_lang=zh-TW")]);
        assert_eq!(resolve(&chosen, Visitor::Anonymous, Some(Lang::En)), Lang::ZhTw);
        assert_eq!(resolve(&headers(&[("accept-language", "zh-HK")]), Visitor::Anonymous, Some(Lang::En)), Lang::ZhTw);
        assert_eq!(resolve(&headers(&[("accept-language", "en-GB")]), Visitor::Anonymous, Some(Lang::ZhTw)), Lang::En);
        assert_eq!(resolve(&headers(&[("accept-language", "fr")]), Visitor::Anonymous, Some(Lang::ZhTw)), Lang::ZhTw);
        assert_eq!(resolve(&HeaderMap::new(), Visitor::Anonymous, None), Lang::En);
        // A cookie that names no language is no choice
        assert_eq!(resolve(&headers(&[("cookie", "tf_lang=fr; tf_lang_chosen=1")]), Visitor::Anonymous, None), Lang::En);
        assert_eq!(picked(&headers(&[("accept-language", "zh-TW")]), Visitor::Anonymous, Some(Lang::En)), None);
    }

    #[test]
    fn emails_follow_the_recipients_saved_language_then_the_default_then_what_they_last_used() {
        assert_eq!(recipient("ja", "zh-TW", Some(Lang::En)), Lang::Ja);
        assert_eq!(recipient("", "zh-TW", Some(Lang::En)), Lang::En);
        assert_eq!(recipient("", "zh-TW", None), Lang::ZhTw);
        assert_eq!(recipient("", "", None), Lang::En);
        assert_eq!(recipient("fr", "", Some(Lang::ZhCn)), Lang::ZhCn);
    }

    #[test]
    fn the_offered_languages_are_the_ones_the_interface_offers() {
        // `ready` of each language in the page's list: a language is offered by both, or by neither
        let source = include_str!("../../../web/src/lib/i18n.ts");
        for l in Lang::ALL {
            let line = source
                .lines()
                .find(|line| line.trim_start().starts_with(&format!("{{ id: \"{}\",", l.code())))
                .unwrap_or_else(|| panic!("{} isn't in LANGUAGES of web/src/lib/i18n.ts", l.code()));
            let ready = line.contains("ready: true");
            assert_eq!(ready, l.offered(), "{}: `ready` in web/src/lib/i18n.ts and `offered` in server/src/i18n/mod.rs differ", l.code());
        }
    }

    #[tokio::test]
    async fn the_chosen_language_is_saved_with_the_account() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let (_, cookie) = env.sign_in(&amy, "Firefox").await;
        let session = headers(&[("cookie", &cookie)]);
        assert!(matches!(visitor(&env.st, &session).await, Visitor::SignedIn(None)));
        assert!(matches!(visitor(&env.st, &headers(&[("cookie", "tf_session=nothing")])).await, Visitor::Anonymous));

        let set = |lang: &str| save(State(env.st.clone()), amy.clone(), Json(LanguageReq { lang: lang.into() }));
        assert!(set("fr").await.is_err());
        assert!(set("zh-tw").await.is_err());
        let _ = set("ja").await.unwrap();
        assert!(matches!(visitor(&env.st, &session).await, Visitor::SignedIn(Some(Lang::Ja))));
        let me = me_lang(&env.st, amy.id, &HeaderMap::new()).await.unwrap();
        assert_eq!((me.lang, me.ui_lang), ("ja", Some("ja")));
        let (chosen, last): (String, String) = sqlx::query_as("SELECT chosen_lang, lang FROM users WHERE id = ?").bind(amy.id).fetch_one(&env.st.db).await.unwrap();
        assert_eq!((chosen.as_str(), last.as_str()), ("ja", "ja"));

        // Forgotten: the system default decides again, and the language last used stays for emails
        let _ = set("").await.unwrap();
        let me = me_lang(&env.st, amy.id, &HeaderMap::new()).await.unwrap();
        assert_eq!((me.lang, me.ui_lang), ("", None));
        env.st.system.write().unwrap().default_lang = "zh-TW".into();
        assert_eq!(me_lang(&env.st, amy.id, &HeaderMap::new()).await.unwrap().ui_lang, Some("zh-TW"));
        let (last,): (String,) = sqlx::query_as("SELECT lang FROM users WHERE id = ?").bind(amy.id).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(last, "ja");
    }
}
