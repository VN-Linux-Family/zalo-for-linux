use napi::Result;
use whatlang::{detect as whatlang_detect, Lang, Script};

pub const MAX_RESULTS: u32 = 16;

pub type Hypothesis = (String, Option<f64>);

pub fn name() -> &'static str {
    "whatlang-linux"
}

pub fn available() -> bool {
    true
}

pub fn score_kind() -> &'static str {
    "probability"
}

/// Convert whatlang Lang and Script into standard BCP 47 language tag
fn map_lang_to_bcp47(lang: Lang, script: Script) -> String {
    match lang {
        Lang::Vie => "vi".to_string(),
        Lang::Eng => "en".to_string(),
        Lang::Cmn => {
            if script == Script::Mandarin {
                "zh-Hans".to_string()
            } else {
                "zh".to_string()
            }
        }
        Lang::Jpn => "ja".to_string(),
        Lang::Kor => "ko".to_string(),
        Lang::Fra => "fr".to_string(),
        Lang::Deu => "de".to_string(),
        Lang::Spa => "es".to_string(),
        Lang::Por => "pt".to_string(),
        Lang::Rus => "ru".to_string(),
        Lang::Ita => "it".to_string(),
        Lang::Tha => "th".to_string(),
        Lang::Ind => "id".to_string(),
        Lang::Ara => "ar".to_string(),
        Lang::Hin => "hi".to_string(),
        Lang::Ukr => "uk".to_string(),
        Lang::Pol => "pl".to_string(),
        Lang::Tur => "tr".to_string(),
        Lang::Nld => "nl".to_string(),
        Lang::Swe => "sv".to_string(),
        Lang::Dan => "da".to_string(),
        Lang::Fin => "fi".to_string(),
        Lang::Ces => "cs".to_string(),
        Lang::Ell => "el".to_string(),
        Lang::Bul => "bg".to_string(),
        Lang::Ron => "ro".to_string(),
        Lang::Hun => "hu".to_string(),
        Lang::Heb => "he".to_string(),
        Lang::Khm => "km".to_string(),
        Lang::Mya => "my".to_string(),
        Lang::Urd => "ur".to_string(),
        Lang::Cat => "ca".to_string(),
        Lang::Hrv => "hr".to_string(),
        Lang::Srp => "sr".to_string(),
        Lang::Slk => "sk".to_string(),
        Lang::Slv => "sl".to_string(),
        _ => lang.code().to_string(),
    }
}

pub fn detect(text: &str, max_results: Option<u32>) -> Result<Vec<Hypothesis>> {
    let _limit = max_results.unwrap_or(MAX_RESULTS).clamp(1, MAX_RESULTS) as usize;

    let trimmed = text.trim();
    if trimmed.chars().count() < 3 {
        return Ok(Vec::new());
    }

    if let Some(info) = whatlang_detect(trimmed) {
        let tag = map_lang_to_bcp47(info.lang(), info.script());
        let confidence = (info.confidence() * 1000.0).round() / 1000.0;
        Ok(vec![(tag, Some(confidence))])
    } else {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata() {
        assert_eq!(name(), "whatlang-linux");
        assert_eq!(available(), true);
        assert_eq!(score_kind(), "probability");
    }

    #[test]
    fn test_detect_vietnamese() {
        let res = detect("Xin chào các bạn, đây là tiếng Việt", None).unwrap();
        assert!(!res.is_empty());
        assert_eq!(res[0].0, "vi");
        assert!(res[0].1.unwrap() > 0.8);
    }

    #[test]
    fn test_detect_english() {
        let res = detect("Hello world, this is an English language sentence.", None).unwrap();
        assert!(!res.is_empty());
        assert_eq!(res[0].0, "en");
        assert!(res[0].1.unwrap() > 0.8);
    }

    #[test]
    fn test_detect_chinese() {
        let res = detect("你好世界，这是一个中文语言测试。", None).unwrap();
        assert!(!res.is_empty());
        assert!(res[0].0 == "zh-Hans" || res[0].0 == "zh");
        assert!(res[0].1.unwrap() > 0.8);
    }

    #[test]
    fn test_detect_japanese() {
        let res = detect("こんにちは世界、日本語のテストです。", None).unwrap();
        assert!(!res.is_empty());
        assert_eq!(res[0].0, "ja");
        assert!(res[0].1.unwrap() > 0.8);
    }

    #[test]
    fn test_detect_short_and_whitespace() {
        assert!(detect("hi", None).unwrap().is_empty());
        assert!(detect("   ", None).unwrap().is_empty());
        assert!(detect("", None).unwrap().is_empty());
    }
}
