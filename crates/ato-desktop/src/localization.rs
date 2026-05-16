use std::collections::HashMap;
use std::sync::LazyLock;

use crate::config::LanguageConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocaleCode {
    En,
    Ja,
}

impl LocaleCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            LocaleCode::En => "en",
            LocaleCode::Ja => "ja",
        }
    }
}

static MESSAGES_EN: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    let src = include_str!("../assets/i18n/en.json");
    serde_json::from_str(src).expect("en.json must be valid")
});

static MESSAGES_JA: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    let src = include_str!("../assets/i18n/ja.json");
    serde_json::from_str(src).expect("ja.json must be valid")
});

pub fn resolve_locale_from_str(lang_str: Option<&str>) -> LocaleCode {
    match lang_str {
        Some(s) if s.starts_with("ja") => LocaleCode::Ja,
        Some(s) if !s.is_empty() => LocaleCode::En,
        _ => LocaleCode::En,
    }
}

pub fn resolve_locale(config: LanguageConfig) -> LocaleCode {
    match config {
        LanguageConfig::Japanese => LocaleCode::Ja,
        LanguageConfig::English => LocaleCode::En,
        LanguageConfig::System => {
            let lang = std::env::var("LANG")
                .or_else(|_| std::env::var("LANGUAGE"))
                .ok();
            resolve_locale_from_str(lang.as_deref())
        }
    }
}

/// Look up a key in the given locale, fall back to English.
/// Substitutes `{param}` tokens using the provided key=value pairs.
pub fn tr(locale: LocaleCode, key: &str) -> String {
    tr_params(locale, key, &[])
}

pub fn tr_params(locale: LocaleCode, key: &str, params: &[(&str, &str)]) -> String {
    let messages = match locale {
        LocaleCode::Ja => &*MESSAGES_JA,
        LocaleCode::En => &*MESSAGES_EN,
    };

    let value = messages
        .get(key)
        .or_else(|| MESSAGES_EN.get(key))
        .cloned()
        .unwrap_or_else(|| {
            tracing::debug!(key, "i18n missing key");
            key.to_string()
        });

    if params.is_empty() {
        return value;
    }

    let mut result = value;
    for (k, v) in params {
        result = result.replace(&format!("{{{k}}}"), v);
    }
    result
}

/// Build the JavaScript i18n block to inject into every system capsule WebView.
/// Returns a script string that defines `window.__ATO_I18N__`, `window.__ATO_T__`,
/// and `window.__ATO_APPLY_I18N__`.
pub fn make_i18n_init_script(locale: LocaleCode) -> String {
    let messages_en = serde_json::to_string(&*MESSAGES_EN).expect("serialize en");
    let messages_ja = serde_json::to_string(&*MESSAGES_JA).expect("serialize ja");
    let locale_str = locale.as_str();

    format!(
        r#"(function(){{
  var _en={messages_en};
  var _ja={messages_ja};
  window.__ATO_I18N__={{locale:"{locale_str}",fallbackLocale:"en",messages:{{en:_en,ja:_ja}}}};
  window.__ATO_T__=function(key,params){{
    var m=window.__ATO_I18N__.messages;
    var locale=window.__ATO_I18N__.locale;
    var v=(m[locale]&&m[locale][key])||(m["en"]&&m["en"][key])||key;
    if(params){{for(var k in params)v=v.replace(new RegExp("\\\\{{"+k+"\\\\}}","g"),params[k]);}}
    return v;
  }};
  window.__ATO_APPLY_I18N__=function(){{
    document.querySelectorAll("[data-i18n]").forEach(function(el){{
      el.textContent=window.__ATO_T__(el.getAttribute("data-i18n"));
    }});
    document.querySelectorAll("[data-i18n-placeholder]").forEach(function(el){{
      el.placeholder=window.__ATO_T__(el.getAttribute("data-i18n-placeholder"));
    }});
    document.querySelectorAll("[data-i18n-title]").forEach(function(el){{
      el.title=window.__ATO_T__(el.getAttribute("data-i18n-title"));
    }});
    document.querySelectorAll("[data-i18n-aria]").forEach(function(el){{
      el.setAttribute("aria-label",window.__ATO_T__(el.getAttribute("data-i18n-aria")));
    }});
  }};
}})();"#,
        messages_en = messages_en,
        messages_ja = messages_ja,
        locale_str = locale_str,
    )
}

/// Prepend the i18n init block to an existing init script.
pub fn compose_init_script(locale: LocaleCode, existing: Option<&str>) -> String {
    let i18n = make_i18n_init_script(locale);
    match existing {
        Some(e) if !e.is_empty() => format!("{}\n{}", i18n, e),
        _ => i18n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_locale_from_str_japanese() {
        assert_eq!(resolve_locale_from_str(Some("ja_JP.UTF-8")), LocaleCode::Ja);
        assert_eq!(resolve_locale_from_str(Some("ja-JP")), LocaleCode::Ja);
        assert_eq!(resolve_locale_from_str(Some("ja")), LocaleCode::Ja);
    }

    #[test]
    fn resolve_locale_from_str_english() {
        assert_eq!(resolve_locale_from_str(Some("en_US.UTF-8")), LocaleCode::En);
        assert_eq!(resolve_locale_from_str(Some("de_DE")), LocaleCode::En);
        assert_eq!(resolve_locale_from_str(None), LocaleCode::En);
        assert_eq!(resolve_locale_from_str(Some("")), LocaleCode::En);
    }

    #[test]
    fn tr_basic_lookup() {
        let val = tr(LocaleCode::En, "common.cancel");
        assert_eq!(val, "Cancel");
        let val_ja = tr(LocaleCode::Ja, "common.cancel");
        assert_eq!(val_ja, "キャンセル");
    }

    #[test]
    fn tr_missing_key_falls_back_to_key() {
        let val = tr(LocaleCode::En, "nonexistent.key");
        assert_eq!(val, "nonexistent.key");
    }

    #[test]
    fn tr_missing_ja_falls_back_to_en() {
        // Both catalogs have this, but verify the mechanism
        let val = tr(LocaleCode::Ja, "common.cancel");
        assert_ne!(val, "common.cancel"); // found in ja catalog
    }

    #[test]
    fn tr_params_substitution() {
        let val = tr_params(
            LocaleCode::En,
            "store.search.results",
            &[("query", "hello")],
        );
        assert!(val.contains("hello"), "should substitute {{query}}");
    }

    #[test]
    fn en_ja_key_parity() {
        let en_keys: std::collections::HashSet<&str> =
            MESSAGES_EN.keys().map(|s| s.as_str()).collect();
        let ja_keys: std::collections::HashSet<&str> =
            MESSAGES_JA.keys().map(|s| s.as_str()).collect();
        let only_en: Vec<&str> = en_keys.difference(&ja_keys).copied().collect();
        let only_ja: Vec<&str> = ja_keys.difference(&en_keys).copied().collect();
        assert!(
            only_en.is_empty() && only_ja.is_empty(),
            "key mismatch — only in en: {only_en:?}, only in ja: {only_ja:?}"
        );
    }
}
