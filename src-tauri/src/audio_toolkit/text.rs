use natural::phonetics::soundex;
use once_cell::sync::Lazy;
use regex::Regex;
use strsim::levenshtein;

/// Builds an n-gram string by cleaning and concatenating words
///
/// Strips punctuation from each word, lowercases, and joins without spaces.
/// This allows matching "Charge B" against "ChargeBee".
fn build_ngram(words: &[&str]) -> String {
    words
        .iter()
        .map(|w| build_match_key(w))
        .collect::<Vec<_>>()
        .concat()
}

fn build_match_key(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

struct CustomWordMatchKey {
    word_index: usize,
    key: String,
}

fn build_custom_word_match_keys(word: &str, word_index: usize) -> Vec<CustomWordMatchKey> {
    let primary_key = build_match_key(word);
    let mut keys = Vec::with_capacity(2);

    // The fallback matcher is intentionally limited to ASCII terms. Its
    // whitespace tokenization and Soundex scoring are not suitable for CJK
    // scripts. Unicode custom words remain available to models that accept
    // them as native decode prompts; they are simply skipped by this fallback.
    if is_supported_fuzzy_key(&primary_key) {
        keys.push(CustomWordMatchKey {
            word_index,
            key: primary_key.clone(),
        });
    }

    if word.contains('&') {
        let expanded_key = build_match_key(&word.replace('&', " and "));
        if is_supported_fuzzy_key(&expanded_key) && expanded_key != primary_key {
            keys.push(CustomWordMatchKey {
                word_index,
                key: expanded_key,
            });
        }
    }

    keys
}

fn is_supported_fuzzy_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric())
}

fn supports_soundex(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphabetic())
}

/// Finds the best matching custom word for a candidate string
///
/// Uses Levenshtein distance and Soundex phonetic matching to find
/// the best match above the given threshold.
///
/// # Arguments
/// * `candidate` - The cleaned/lowercased candidate string to match
/// * `custom_words` - Original custom words (for returning the replacement)
/// * `custom_word_match_keys` - Normalized custom-word keys for comparison
/// * `threshold` - Maximum similarity score to accept
///
/// # Returns
/// The best matching custom word and its score, if any match was found
fn find_best_match<'a>(
    candidate: &str,
    custom_words: &'a [String],
    custom_word_match_keys: &[CustomWordMatchKey],
    threshold: f64,
) -> Option<(&'a String, f64)> {
    if !is_supported_fuzzy_key(candidate) || candidate.chars().count() > 50 {
        return None;
    }

    let mut best_match: Option<&String> = None;
    let mut best_score = f64::MAX;

    for custom_word_key in custom_word_match_keys {
        // Skip if lengths are too different (optimization + prevents over-matching)
        // Use percentage-based check: max 25% length difference (prevents n-grams from
        // matching significantly shorter custom words, e.g., "openaigpt" vs "openai")
        let candidate_len = candidate.chars().count();
        let custom_word_len = custom_word_key.key.chars().count();
        let len_diff = candidate_len.abs_diff(custom_word_len) as f64;
        let max_len = candidate_len.max(custom_word_len) as f64;
        let max_allowed_diff = (max_len * 0.25).max(2.0); // At least 2 chars difference allowed
        if len_diff > max_allowed_diff {
            continue;
        }

        // Calculate Levenshtein distance (normalized by length)
        let levenshtein_dist = levenshtein(candidate, &custom_word_key.key);
        let levenshtein_score = if max_len > 0.0 {
            levenshtein_dist as f64 / max_len
        } else {
            1.0
        };

        // Soundex is an English/ASCII phonetic algorithm. Numeric terms can
        // still use edit distance, but must not receive a phonetic boost.
        let phonetic_match = supports_soundex(candidate)
            && supports_soundex(&custom_word_key.key)
            && soundex(candidate, &custom_word_key.key);

        // Combine scores: favor phonetic matches, but also consider string similarity
        let combined_score = if phonetic_match {
            levenshtein_score * 0.3 // Give significant boost to phonetic matches
        } else {
            levenshtein_score
        };

        // Accept if the score is good enough (configurable threshold)
        if combined_score < threshold && combined_score < best_score {
            best_match = Some(&custom_words[custom_word_key.word_index]);
            best_score = combined_score;
        }
    }

    best_match.map(|m| (m, best_score))
}

/// Applies custom word corrections to transcribed text using fuzzy matching
///
/// This function corrects words in the input text by finding the best matches
/// from a list of custom words using a combination of:
/// - Levenshtein distance for string similarity
/// - Soundex phonetic matching for pronunciation similarity
/// - N-gram matching for multi-word speech artifacts (e.g., "Charge B" -> "ChargeBee")
///
/// # Arguments
/// * `text` - The input text to correct
/// * `custom_words` - List of custom words to match against
/// * `threshold` - Maximum similarity score to accept (0.0 = exact match, 1.0 = any match)
///
/// # Returns
/// The corrected text with custom words applied
pub fn apply_custom_words(text: &str, custom_words: &[String], threshold: f64) -> String {
    if custom_words.is_empty() {
        return text.to_string();
    }

    // Pre-compute normalized comparison keys to avoid repeated allocations.
    let custom_word_match_keys: Vec<CustomWordMatchKey> = custom_words
        .iter()
        .enumerate()
        .flat_map(|(index, word)| build_custom_word_match_keys(word, index))
        .collect();

    let words: Vec<&str> = text.split_whitespace().collect();
    let mut result = Vec::new();
    let mut i = 0;

    while i < words.len() {
        let mut best_match: Option<(usize, &String, f64)> = None;

        // Consider n-grams up to three words and choose the closest match. A
        // longest-first match can consume a following ordinary word when both
        // candidates happen to share a Soundex code (for example,
        // "Charge B, che" matching "ChargeBee").
        for n in (1..=3).rev() {
            if i + n > words.len() {
                continue;
            }

            let ngram_words = &words[i..i + n];
            // Do not consume across a punctuation boundary. In
            // "Charge B, che", the comma closes the candidate at "B,".
            if ngram_words[..n.saturating_sub(1)]
                .iter()
                .any(|word| !extract_punctuation(word).1.is_empty())
            {
                continue;
            }
            let ngram = build_ngram(ngram_words);

            if let Some((replacement, score)) =
                find_best_match(&ngram, custom_words, &custom_word_match_keys, threshold)
            {
                let is_better = best_match
                    .as_ref()
                    .is_none_or(|(_, _, best_score)| score < *best_score);
                if is_better {
                    best_match = Some((n, replacement, score));
                }
            }
        }

        if let Some((n, replacement, _)) = best_match {
            let ngram_words = &words[i..i + n];
            // Extract punctuation from first and last words of the n-gram.
            let (prefix, _) = extract_punctuation(ngram_words[0]);
            let (_, suffix) = extract_punctuation(ngram_words[n - 1]);

            // Preserve case from first word.
            let corrected = preserve_case_pattern(ngram_words[0], replacement);

            result.push(format!("{}{}{}", prefix, corrected, suffix));
            i += n;
        } else {
            result.push(words[i].to_string());
            i += 1;
        }
    }

    result.join(" ")
}

/// Preserves the case pattern of the original word when applying a replacement
fn preserve_case_pattern(original: &str, replacement: &str) -> String {
    if original.chars().all(|c| c.is_uppercase()) {
        replacement.to_uppercase()
    } else if original.chars().next().is_some_and(|c| c.is_uppercase()) {
        let mut chars: Vec<char> = replacement.chars().collect();
        if let Some(first_char) = chars.get_mut(0) {
            *first_char = first_char.to_uppercase().next().unwrap_or(*first_char);
        }
        chars.into_iter().collect()
    } else {
        replacement.to_string()
    }
}

/// Extracts punctuation prefix and suffix from a word
fn extract_punctuation(word: &str) -> (&str, &str) {
    // String slices use byte offsets. Derive both boundaries from char_indices
    // so multibyte punctuation such as `。` and `「」` can never be split.
    let prefix_end = word
        .char_indices()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(index, _)| index)
        .unwrap_or(word.len());
    let suffix_start = word
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(index, c)| index + c.len_utf8())
        .unwrap_or(0);

    let prefix = if prefix_end > 0 {
        &word[..prefix_end]
    } else {
        ""
    };

    let suffix = if suffix_start < word.len() {
        &word[suffix_start..]
    } else {
        ""
    };

    (prefix, suffix)
}

/// Evidence for the language of the text being cleaned.
///
/// This intentionally describes the transcription output, not Handy's UI
/// language. Unknown output languages fail closed: built-in filler removal is
/// skipped rather than applying a language profile speculatively.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputLanguageEvidence {
    UserSelected(String),
    ModelConstrained(String),
    /// The transcription model itself identified the language (audio-based
    /// LID, e.g. Whisper in auto mode).
    ModelDetected(String),
    /// Handy's spoken-language detector picked it, and the model ran with it.
    Detected(String),
    /// Detected from the transcribed text with high confidence, constrained to
    /// the model's supported languages. Weakest accepted evidence.
    TextDetected(String),
    TranslatedToEnglish,
    Unknown,
}

impl OutputLanguageEvidence {
    pub(crate) fn language(&self) -> Option<&str> {
        match self {
            Self::UserSelected(language)
            | Self::ModelConstrained(language)
            | Self::ModelDetected(language)
            | Self::Detected(language)
            | Self::TextDetected(language) => Some(language),
            Self::TranslatedToEnglish => Some("en"),
            Self::Unknown => None,
        }
    }
}

/// Filler tokens that are not lexical words in any language Handy's models can
/// output, so removing them cannot corrupt text regardless of the (possibly
/// unknown) output language. Kept deliberately conservative: anything that is a
/// real word somewhere ("um" pt/de, "ha" es, "ah"/"eh" interjections, "mm"
/// millimetres) belongs in the language-gated lists instead.
const UNIVERSAL_FILLER_WORDS: &[&str] = &[
    "uh", "uhm", "umm", "uhh", "uhhh", "ehh", "ehm", "ahm", "hmm", "hm", "mmm", "хм", "ммм",
];

/// Filler words that are only safe to remove with evidence for the output
/// language, because the same token is a real word elsewhere (e.g. Portuguese
/// "um" = "a/an", German "um" = "at/around", Spanish "ha" = "has").
fn gated_filler_words_for_language(lang: &str) -> &'static [&'static str] {
    let base_lang = lang.split(&['-', '_'][..]).next().unwrap_or(lang);

    match base_lang {
        "en" => &["um", "ah", "eh"],
        "de" => &["äh", "ähm"],
        "fr" => &["euh"],
        _ => &[],
    }
}

static MULTI_SPACE_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s{2,}").unwrap());

/// Collapses repeated words (3+ repetitions) to a single instance.
/// E.g., "wh wh wh wh" -> "wh", "I I I I" -> "I"
fn collapse_stutters(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return text.to_string();
    }

    let mut result: Vec<&str> = Vec::new();
    let mut i = 0;

    while i < words.len() {
        let word = words[i];
        let word_lower = word.to_lowercase();

        if word_lower.chars().all(|c| c.is_alphabetic()) {
            // Count consecutive repetitions (case-insensitive)
            let mut count = 1;
            while i + count < words.len() && words[i + count].to_lowercase() == word_lower {
                count += 1;
            }

            // If 3+ repetitions, collapse to single instance
            if count >= 3 {
                result.push(word);
                i += count;
            } else {
                result.push(word);
                i += 1;
            }
        } else {
            result.push(word);
            i += 1;
        }
    }

    result.join(" ")
}

/// Whether a word appended to `kept` would open a sentence: nothing but
/// whitespace so far, or the last visible character ends a sentence.
fn opens_sentence(kept: &str) -> bool {
    kept.trim_end()
        .chars()
        .next_back()
        .is_none_or(|c| matches!(c, '.' | '!' | '?' | '…'))
}

/// Appends `segment` to `kept`. While `capital_owed` is set, the first
/// alphanumeric character of `segment` is uppercased and the debt is settled.
fn push_restoring_capital(kept: &mut String, segment: &str, capital_owed: &mut bool) {
    if *capital_owed {
        if let Some((index, first)) = segment.char_indices().find(|(_, c)| c.is_alphanumeric()) {
            *capital_owed = false;
            kept.push_str(&segment[..index]);
            kept.extend(first.to_uppercase());
            kept.push_str(&segment[index + first.len_utf8()..]);
            return;
        }
    }
    kept.push_str(segment);
}

/// Deletes every match of one filler pattern. A capitalized filler that opened
/// a sentence hands its capital to the word that takes its place, so
/// "Um, so I think" becomes "So I think" rather than "so I think".
fn remove_filler_matches(text: &str, pattern: &Regex) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut resume = 0;
    let mut capital_owed = false;

    for filler in pattern.find_iter(text) {
        push_restoring_capital(&mut kept, &text[resume..filler.start()], &mut capital_owed);
        let capitalized = filler.as_str().starts_with(char::is_uppercase);
        capital_owed |= capitalized && opens_sentence(&kept);
        resume = filler.end();
    }
    push_restoring_capital(&mut kept, &text[resume..], &mut capital_owed);

    kept
}

/// Removes filler words from transcription output when enabled.
///
/// Built-in removal is two-tiered: [`UNIVERSAL_FILLER_WORDS`] apply regardless
/// of language evidence, while [`gated_filler_words_for_language`] tokens are
/// only removed when the output language is known. A custom list is an
/// explicit user override and replaces both tiers without requiring language
/// evidence. `Some(empty vec)` disables removal, preserving the legacy
/// power-user setting. The master toggle takes precedence over both built-in
/// and custom lists.
///
/// # Arguments
/// * `text` - The raw transcription text to filter
/// * `language` - Evidence for the language of the transcription output
/// * `custom_filler_words` - Optional user-provided filler word list. `Some(vec)` overrides
///   language defaults; `Some(empty vec)` disables filtering; `None` uses language defaults.
/// * `enabled` - Whether filler-word removal is enabled
///
/// # Returns
/// The text with configured filler words removed
pub fn remove_filler_words(
    text: &str,
    language: &OutputLanguageEvidence,
    custom_filler_words: &Option<Vec<String>>,
    enabled: bool,
) -> String {
    if !enabled {
        return text.to_string();
    }

    // Build filler patterns from custom list or the built-in tiers
    let patterns: Vec<Regex> = match custom_filler_words {
        Some(words) => words
            .iter()
            .filter_map(|word| Regex::new(&format!(r"(?i)\b{}\b[,.]?", regex::escape(word))).ok())
            .collect(),
        None => UNIVERSAL_FILLER_WORDS
            .iter()
            .chain(
                language
                    .language()
                    .map(gated_filler_words_for_language)
                    .unwrap_or_default(),
            )
            .map(|word| Regex::new(&format!(r"(?i)\b{}\b[,.]?", regex::escape(word))).unwrap())
            .collect(),
    };

    // Remove filler words
    let mut filtered = text.to_string();
    for pattern in &patterns {
        filtered = remove_filler_matches(&filtered, pattern);
    }

    filtered
}

/// Applies non-filler transcription cleanup.
///
/// Kept separate from [`remove_filler_words`] so disabling filler deletion
/// does not also disable the existing repeated-word and whitespace cleanup.
pub fn normalize_transcription_output(text: &str) -> String {
    let mut normalized = collapse_stutters(text);

    // Clean up multiple spaces to single space
    normalized = MULTI_SPACE_PATTERN
        .replace_all(&normalized, " ")
        .to_string();

    // Trim leading/trailing whitespace
    normalized.trim().to_string()
}

/// Punctuation that ends a clause, ends a sentence, or stops one, and what
/// may follow a spoken list marker ("Number one," or "Bullet point:").
const CLAUSE_END: [char; 7] = ['.', '!', '?', ',', ';', ':', '…'];
const SENTENCE_END: [char; 5] = ['.', '!', '?', ':', '…'];
const FULL_STOP: [char; 4] = ['.', '!', '?', '…'];
const MARKER_TAIL: [char; 3] = [',', ':', '.'];

const NUMBER_WORDS: [&str; 10] = [
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
];

/// The number a spoken list marker counts: "one" to "ten", or the digits
/// Parakeet writes when it hears a list.
fn spoken_number(core: &str) -> Option<usize> {
    let word = NUMBER_WORDS.iter().position(|word| *word == core);
    word.map(|index| index + 1).or_else(|| {
        let number: usize = core.parse().ok()?;
        ((1..=10).contains(&number) && number.to_string() == core).then_some(number)
    })
}

/// One word of a dictation, split from the punctuation around it.
struct SpokenToken<'a> {
    raw: &'a str,
    lead: &'a str,
    /// The word without its punctuation, lowercased.
    core: String,
    tail: &'a str,
}

impl<'a> SpokenToken<'a> {
    fn new(raw: &'a str) -> Self {
        let (lead, tail) = extract_punctuation(raw);
        let core = raw.strip_prefix(lead).and_then(|w| w.strip_suffix(tail));
        match core.filter(|core| !core.is_empty()) {
            Some(core) => Self {
                raw,
                lead,
                core: core.to_lowercase(),
                tail,
            },
            // Punctuation only.
            None => Self {
                raw,
                lead: raw,
                core: String::new(),
                tail: "",
            },
        }
    }

    fn ends_with_any(&self, marks: &[char]) -> bool {
        self.tail.chars().any(|c| marks.contains(&c))
    }

    /// Whether the punctuation after the word is what follows a list marker.
    fn has_marker_tail(&self) -> bool {
        !self.tail.is_empty() && self.tail.chars().all(|c| MARKER_TAIL.contains(&c))
    }
}

enum SpokenPiece {
    Text(String),
    /// A line break (1) or a paragraph break (2).
    Break(usize),
    /// A list marker: its number, or `None` for a bullet, and the words it
    /// was spoken as.
    Marker(Option<usize>, String),
}

/// Where the bullet phrase that starts at token `i` ends: "bullet point",
/// "next bullet", "new bullet", "next bullet point" or "new bullet point",
/// with something after it.
fn bullet_phrase_end(tokens: &[SpokenToken], i: usize) -> Option<usize> {
    let (first, second) = (tokens.get(i)?, tokens.get(i + 1)?);
    if !first.lead.is_empty() || !first.tail.is_empty() || !second.lead.is_empty() {
        return None;
    }
    let mut end = match (first.core.as_str(), second.core.as_str()) {
        ("bullet", "point") | ("next" | "new", "bullet") => i + 1,
        _ => return None,
    };
    let third = tokens.get(i + 2);
    if second.core == "bullet"
        && second.tail.is_empty()
        && third.is_some_and(|third| third.lead.is_empty() && third.core == "point")
    {
        end = i + 2;
    }
    let last = tokens.get(end)?;
    let next = tokens.get(end + 1)?;
    // "Bullet point two is wrong" names a bullet; it does not start one.
    let names_a_bullet = last.tail.is_empty()
        && (NUMBER_WORDS.contains(&next.core.as_str())
            || (!next.core.is_empty() && next.core.chars().all(|c| c.is_ascii_digit())));
    let own_phrase = last.tail.is_empty() || last.has_marker_tail();
    (own_phrase && !names_a_bullet).then_some(end)
}

/// The line or paragraph break spoken at token `i`: how many newlines, and
/// the token after it.
fn spoken_break_at(tokens: &[SpokenToken], i: usize, clause_start: bool) -> Option<(usize, usize)> {
    let token = tokens.get(i)?;
    let is_last = |index: usize| index + 1 == tokens.len();
    if token.core == "new" {
        let next = tokens.get(i + 1)?;
        let breaks = match next.core.as_str() {
            "line" => 1,
            "paragraph" => 2,
            _ => return None,
        };
        let own_phrase = is_last(i + 1) || next.ends_with_any(&CLAUSE_END);
        let found = clause_start && token.tail.is_empty() && next.lead.is_empty() && own_phrase;
        return found.then_some((breaks, i + 2));
    }
    if token.core == "newline" {
        let own_phrase = token.ends_with_any(&CLAUSE_END);
        // Said without a pause, Parakeet still sets the command off as one
        // capitalised word: "Hi John Newline, thanks".
        let set_off = own_phrase && token.raw.starts_with('N');
        if (clause_start && (own_phrase || is_last(i))) || set_off {
            return Some((1, i + 1));
        }
    }
    None
}

/// The numbered marker spoken at token `i`: its number, and the token after
/// it. "Number one," counts at any clause start. Whether a bare "One," or
/// "1." counts here is the caller's `bare_allowed`.
fn spoken_number_at(
    tokens: &[SpokenToken],
    i: usize,
    clause_start: bool,
    bare_allowed: bool,
) -> Option<(usize, usize)> {
    let token = tokens.get(i)?;
    let (numeral, next) = if clause_start && token.core == "number" && token.tail.is_empty() {
        (tokens.get(i + 1).filter(|t| t.lead.is_empty())?, i + 2)
    } else if bare_allowed {
        (token, i + 1)
    } else {
        return None;
    };
    let number = spoken_number(&numeral.core)?;
    (numeral.has_marker_tail() && next < tokens.len()).then_some((number, next))
}

/// Splits a dictation into text and the commands spoken as their own phrase.
fn scan_spoken_commands(tokens: &[SpokenToken]) -> Vec<SpokenPiece> {
    let starts_after = |i: usize, marks: &[char]| match i.checked_sub(1) {
        Some(previous) => tokens.get(previous).is_some_and(|t| t.ends_with_any(marks)),
        None => true,
    };
    let spoken = |start: usize, next: usize| {
        let words = tokens.get(start..next).unwrap_or_default();
        words.iter().map(|t| t.raw).collect::<Vec<_>>().join(" ")
    };

    // A bullet list has at least two bullets, each at the start of a clause
    // ("Shopping list, bullet point milk, bullet point eggs"). One "bullet
    // point" alone is taken as words: "Bullet point formatting works well."
    let bullets = (0..tokens.len())
        .filter(|&i| starts_after(i, &CLAUSE_END) && bullet_phrase_end(tokens, i).is_some());
    let bullet_seen = bullets.count() >= 2;
    let mut number_seen = false;

    let mut pieces = Vec::new();
    let mut words: Vec<&str> = Vec::new();
    let mut i = 0;
    while let Some(token) = tokens.get(i) {
        let clause_start = starts_after(i, &CLAUSE_END);
        let sentence_start = starts_after(i, &SENTENCE_END);
        let bullet_allowed = clause_start && bullet_seen;
        // Inside a list only the number as Parakeet writes it counts after a
        // comma ("1. The price is too high, 2. The delivery is too slow"), so
        // "sizes one, two, and three" stays in its item.
        let written_number = token.tail == "." && token.core.chars().all(|c| c.is_ascii_digit());
        let bare_allowed = sentence_start || (clause_start && number_seen && written_number);

        let command = if !token.lead.is_empty() {
            None
        } else if let Some((breaks, next)) = spoken_break_at(tokens, i, clause_start) {
            Some((SpokenPiece::Break(breaks), next))
        } else if let Some(end) = bullet_phrase_end(tokens, i).filter(|_| bullet_allowed) {
            Some((SpokenPiece::Marker(None, spoken(i, end + 1)), end + 1))
        } else if let Some((number, next)) = spoken_number_at(tokens, i, clause_start, bare_allowed)
        {
            number_seen = true;
            Some((SpokenPiece::Marker(Some(number), spoken(i, next)), next))
        } else {
            None
        };

        let Some((piece, next)) = command else {
            words.push(token.raw);
            i += 1;
            continue;
        };
        if !words.is_empty() {
            pieces.push(SpokenPiece::Text(words.join(" ")));
            words.clear();
        }
        pieces.push(piece);
        i = next;
    }
    if !words.is_empty() {
        pieces.push(SpokenPiece::Text(words.join(" ")));
    }
    pieces
}

/// Turns every marker that is not part of a list back into the words it was
/// spoken as. A bullet needs text after it. A numbered list counts up from
/// one, has text after every marker, and has at least two markers.
fn reject_stray_markers(pieces: Vec<SpokenPiece>) -> Vec<SpokenPiece> {
    let has_text = |index: usize| match pieces.get(index) {
        Some(SpokenPiece::Text(text)) => text.chars().any(char::is_alphanumeric),
        _ => false,
    };
    let number_at = |index: usize| match pieces.get(index) {
        Some(SpokenPiece::Marker(number, _)) => *number,
        _ => None,
    };

    let mut stray = Vec::new();
    let mut j = 0;
    while let Some(piece) = pieces.get(j) {
        match piece {
            SpokenPiece::Marker(None, _) if !has_text(j + 1) => stray.push(j),
            SpokenPiece::Marker(Some(_), _) => {
                // The markers from here on that count up from one.
                let mut run = Vec::new();
                let mut complete = true;
                while complete && number_at(j + 2 * run.len()) == Some(run.len() + 1) {
                    let at = j + 2 * run.len();
                    run.push(at);
                    complete = has_text(at + 1);
                }
                let last = run.last().copied().unwrap_or(j);
                if !complete || run.len() < 2 {
                    stray.push(j);
                    stray.extend(run);
                }
                j = last;
            }
            _ => {}
        }
        j += 1;
    }

    let mut merged: Vec<SpokenPiece> = Vec::new();
    for (index, piece) in pieces.into_iter().enumerate() {
        let piece = match piece {
            SpokenPiece::Marker(_, spoken) if stray.contains(&index) => SpokenPiece::Text(spoken),
            other => other,
        };
        match (merged.last_mut(), piece) {
            (Some(SpokenPiece::Text(previous)), SpokenPiece::Text(more)) => {
                previous.push(' ');
                previous.push_str(&more);
            }
            (_, piece) => merged.push(piece),
        }
    }
    merged
}

/// Ends the line `out` is on: `add` more newlines, or enough to have
/// `at_least`, never more than a blank line. Nothing is added before the
/// first text.
fn break_line(out: &mut String, add: usize, at_least: usize) {
    while out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() {
        return;
    }
    let have = out.chars().rev().take_while(|c| *c == '\n').count();
    for _ in have..(have + add).max(at_least).min(2) {
        out.push('\n');
    }
}

fn render_spoken(pieces: &[SpokenPiece]) -> String {
    let mut out = String::new();
    let mut capital_owed = false;
    let mut j = 0;
    while let Some(piece) = pieces.get(j) {
        j += 1;
        match (piece, pieces.get(j)) {
            (SpokenPiece::Break(breaks), _) => {
                break_line(&mut out, *breaks, 0);
                capital_owed = true;
            }
            (SpokenPiece::Marker(number, _), Some(SpokenPiece::Text(text))) => {
                j += 1;
                let list_goes_on = match pieces.get(j) {
                    Some(SpokenPiece::Marker(next, _)) => next.is_some() == number.is_some(),
                    _ => false,
                };
                // The last item of a list is its first sentence; what follows
                // is ordinary text again.
                let words: Vec<&str> = text.split_whitespace().collect();
                let stop = words
                    .iter()
                    .position(|word| SpokenToken::new(word).ends_with_any(&FULL_STOP));
                let item_len = stop
                    .filter(|_| !list_goes_on)
                    .map_or(words.len(), |stop| stop + 1);
                let item = words.get(..item_len).unwrap_or_default().join(" ");
                let rest = words.get(item_len..).unwrap_or_default().join(" ");

                break_line(&mut out, 0, 1);
                out.push_str(&number.map_or("- ".to_string(), |number| format!("{number}. ")));
                let item = item.trim_end_matches(['.', ',', ';', ':', ' ']);
                push_restoring_capital(&mut out, item, &mut true);
                if !rest.is_empty() {
                    break_line(&mut out, 0, 1);
                    push_restoring_capital(&mut out, &rest, &mut true);
                }
                capital_owed = false;
            }
            (SpokenPiece::Text(text) | SpokenPiece::Marker(_, text), _) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push(' ');
                }
                push_restoring_capital(&mut out, text, &mut capital_owed);
                capital_owed = false;
            }
        }
    }
    out.trim().to_string()
}

/// Turns the formatting commands of an English dictation into formatting:
/// "new line", "new paragraph", "bullet point ..." and "number one, ...
/// number two, ...". A command only counts as its own phrase, set off by the
/// punctuation the model wrote, so "a new line of products" stays text. Text
/// without a command comes back byte for byte.
///
/// With `multiline` off the result is one line: breaks become a space and
/// list items are joined with "; ".
pub fn format_spoken(text: &str, multiline: bool) -> String {
    if text.contains('\n') {
        return text.to_string();
    }
    let tokens: Vec<SpokenToken> = text.split_whitespace().map(SpokenToken::new).collect();
    let pieces = reject_stray_markers(scan_spoken_commands(&tokens));
    if matches!(pieces.as_slice(), [] | [SpokenPiece::Text(_)]) {
        return text.to_string();
    }
    let formatted = render_spoken(&pieces);
    if formatted.is_empty() {
        // Nothing but commands ("New line."): paste the words, not nothing.
        return text.to_string();
    }
    if multiline {
        return formatted;
    }

    let mut line = String::new();
    for part in formatted.lines().filter(|part| !part.is_empty()) {
        if let Some(last) = line.chars().next_back() {
            line.push_str(if CLAUSE_END.contains(&last) {
                " "
            } else {
                "; "
            });
        }
        line.push_str(part);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercise the complete cleanup sequence with an explicitly selected
    /// language. Individual tests below predate the split between filler
    /// removal and non-filler normalization.
    fn filter_transcription_output(
        text: &str,
        language: &str,
        custom_filler_words: &Option<Vec<String>>,
    ) -> String {
        let language = OutputLanguageEvidence::UserSelected(language.to_string());
        let filtered = remove_filler_words(text, &language, custom_filler_words, true);
        normalize_transcription_output(&filtered)
    }

    #[test]
    fn test_apply_custom_words_exact_match() {
        let text = "hello world";
        let custom_words = vec!["Hello".to_string(), "World".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn test_apply_custom_words_fuzzy_match() {
        let text = "helo wrold";
        let custom_words = vec!["hello".to_string(), "world".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_preserve_case_pattern() {
        assert_eq!(preserve_case_pattern("HELLO", "world"), "WORLD");
        assert_eq!(preserve_case_pattern("Hello", "world"), "World");
        assert_eq!(preserve_case_pattern("hello", "WORLD"), "WORLD");
    }

    #[test]
    fn test_extract_punctuation() {
        assert_eq!(extract_punctuation("hello"), ("", ""));
        assert_eq!(extract_punctuation("!hello?"), ("!", "?"));
        assert_eq!(extract_punctuation("...hello..."), ("...", "..."));
    }

    #[test]
    fn test_extract_punctuation_uses_unicode_boundaries() {
        assert_eq!(extract_punctuation("你好。"), ("", "。"));
        assert_eq!(extract_punctuation("「你好」"), ("「", "」"));
        assert_eq!(extract_punctuation("你好！"), ("", "！"));
    }

    #[test]
    fn test_empty_custom_words() {
        let text = "hello world";
        let custom_words = vec![];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_filter_filler_words() {
        let text = "So uhm I was thinking uh about this";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "So I was thinking about this");
    }

    #[test]
    fn test_filter_filler_words_case_insensitive() {
        let text = "UHM this is UH a test";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "This is a test");
    }

    #[test]
    fn test_filter_filler_words_with_punctuation() {
        let text = "Well, uhm, I think, uh. that's right";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Well, I think, that's right");
    }

    #[test]
    fn test_filter_cleans_whitespace() {
        let text = "Hello    world   test";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Hello world test");
    }

    #[test]
    fn test_filter_trims() {
        let text = "  Hello world  ";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_filter_combined() {
        let text = "  Uhm, so I was, uh, thinking about this  ";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "So I was, thinking about this");
    }

    #[test]
    fn test_filter_leading_filler_keeps_sentence_capital() {
        let result = filter_transcription_output("Um, so I think we should ship it.", "en", &None);
        assert_eq!(result, "So I think we should ship it.");

        let result = filter_transcription_output("That works. Um, let me check.", "en", &None);
        assert_eq!(result, "That works. Let me check.");

        // Mid-sentence there is no capital to hand over.
        let result = filter_transcription_output("He said, Um, not today.", "en", &None);
        assert_eq!(result, "He said, not today.");
    }

    #[test]
    fn test_filter_preserves_valid_text() {
        let text = "This is a completely normal sentence.";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "This is a completely normal sentence.");
    }

    #[test]
    fn test_filter_stutter_collapse() {
        let text = "w wh wh wh wh wh wh wh wh wh why";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "w wh why");
    }

    #[test]
    fn test_filter_stutter_short_words() {
        let text = "I I I I think so so so so";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "I think so");
    }

    #[test]
    fn test_filter_stutter_longer_words() {
        let text = "Check data doc doc doc doc documentation.";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Check data doc documentation.");
    }

    #[test]
    fn test_filter_stutter_mixed_case() {
        let text = "No NO no NO no";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "No");
    }

    #[test]
    fn test_filter_stutter_preserves_two_repetitions() {
        let text = "no no is fine";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "no no is fine");
    }

    #[test]
    fn test_filter_english_removes_um() {
        let text = "um I think um this is good";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "I think this is good");
    }

    #[test]
    fn test_filter_portuguese_preserves_um() {
        // "um" means "a/an" in Portuguese
        let text = "um gato bonito";
        let result = filter_transcription_output(text, "pt", &None);
        assert_eq!(result, "um gato bonito");
    }

    #[test]
    fn test_filter_spanish_preserves_ha() {
        // "ha" means "has" in Spanish
        let text = "ha sido un buen día";
        let result = filter_transcription_output(text, "es", &None);
        assert_eq!(result, "ha sido un buen día");
    }

    #[test]
    fn test_filter_language_code_with_region() {
        // "pt-BR" should normalize to "pt"
        let text = "um gato bonito";
        let result = filter_transcription_output(text, "pt-BR", &None);
        assert_eq!(result, "um gato bonito");
    }

    #[test]
    fn test_filter_custom_filler_words_override() {
        let custom = Some(vec!["okay".to_string(), "right".to_string()]);
        let text = "okay so I think right this works";
        let result = filter_transcription_output(text, "en", &custom);
        assert_eq!(result, "so I think this works");
    }

    #[test]
    fn test_filter_custom_filler_words_empty_disables() {
        let custom = Some(vec![]);
        let text = "So uhm I was thinking uh about this";
        let result = filter_transcription_output(text, "en", &custom);
        // No filler words removed since custom list is empty
        assert_eq!(result, "So uhm I was thinking uh about this");
    }

    #[test]
    fn test_filter_unknown_language_still_removes_universal_fillers() {
        let text = "uh I think uhm this works";
        let result = filter_transcription_output(text, "xx", &None);
        assert_eq!(result, "I think this works");
    }

    #[test]
    fn test_filter_unknown_language_does_not_remove_um() {
        let text = "um I think this works";
        let result = filter_transcription_output(text, "xx", &None);
        assert_eq!(result, "um I think this works");
    }

    #[test]
    fn test_filter_unknown_evidence_removes_universal_keeps_gated() {
        let filtered = remove_filler_words(
            "uhh bueno hmm creo que um ha llegado",
            &OutputLanguageEvidence::Unknown,
            &None,
            true,
        );
        assert_eq!(
            normalize_transcription_output(&filtered),
            "bueno creo que um ha llegado"
        );

        let cyrillic = remove_filler_words(
            "хм я думаю ммм это работает",
            &OutputLanguageEvidence::Unknown,
            &None,
            true,
        );
        assert_eq!(
            normalize_transcription_output(&cyrillic),
            "я думаю это работает"
        );
    }

    #[test]
    fn test_filter_german_gated_fillers_require_evidence() {
        let text = "äh ich glaube ähm das passt";

        let unknown = remove_filler_words(text, &OutputLanguageEvidence::Unknown, &None, true);
        assert_eq!(normalize_transcription_output(&unknown), text);

        let result = filter_transcription_output(text, "de", &None);
        assert_eq!(result, "ich glaube das passt");
    }

    #[test]
    fn test_filter_preserves_millimetre_unit() {
        // "mm" was removed from the filler lists because it eats units.
        let text = "the screw is 5 mm long";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "the screw is 5 mm long");
    }

    #[test]
    fn test_filter_detected_evidence_unlocks_gated_fillers() {
        let model = remove_filler_words(
            "um I think this works",
            &OutputLanguageEvidence::ModelDetected("en".to_string()),
            &None,
            true,
        );
        assert_eq!(normalize_transcription_output(&model), "I think this works");

        let text = remove_filler_words(
            "euh je pense que ça marche",
            &OutputLanguageEvidence::TextDetected("fr".to_string()),
            &None,
            true,
        );
        assert_eq!(
            normalize_transcription_output(&text),
            "je pense que ça marche"
        );
    }

    #[test]
    fn test_filter_master_toggle_disables_custom_and_builtin_removal() {
        let text = "um customword I think";
        let language = OutputLanguageEvidence::UserSelected("en".to_string());
        let custom = Some(vec!["customword".to_string()]);

        let result = remove_filler_words(text, &language, &custom, false);

        assert_eq!(result, text);
    }

    #[test]
    fn test_filter_custom_words_apply_without_language_evidence() {
        let custom = Some(vec!["customword".to_string()]);
        let text = "customword should be removed but um should remain";

        let filtered = remove_filler_words(text, &OutputLanguageEvidence::Unknown, &custom, true);
        let result = normalize_transcription_output(&filtered);

        assert_eq!(result, "should be removed but um should remain");
    }

    #[test]
    fn test_apply_custom_words_ngram_two_words() {
        let text = "il cui nome è Charge B, che permette";
        let custom_words = vec!["ChargeBee".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert!(result.contains("ChargeBee,"), "unexpected result: {result}");
        assert!(!result.contains("Charge B"));
    }

    #[test]
    fn test_apply_custom_words_ngram_three_words() {
        let text = "use Chat G P T for this";
        let custom_words = vec!["ChatGPT".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert!(result.contains("ChatGPT"));
    }

    #[test]
    fn test_apply_custom_words_prefers_longer_ngram() {
        let text = "Open AI GPT model";
        let custom_words = vec!["OpenAI".to_string(), "GPT".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "OpenAI GPT model");
    }

    #[test]
    fn test_apply_custom_words_ngram_preserves_case() {
        let text = "CHARGE B is great";
        let custom_words = vec!["ChargeBee".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert!(result.contains("CHARGEBEE"));
    }

    #[test]
    fn test_apply_custom_words_ngram_with_spaces_in_custom() {
        // Custom word with space should also match against split words
        let text = "using Mac Book Pro";
        let custom_words = vec!["MacBook Pro".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "using MacBook Pro");
    }

    #[test]
    fn test_apply_custom_words_trailing_number_not_doubled() {
        // Verify that trailing non-alpha chars (like numbers) aren't double-counted
        // between build_ngram stripping them and extract_punctuation capturing them
        let text = "use GPT4 for this";
        let custom_words = vec!["GPT-4".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        // Should NOT produce "GPT-44" (double-counting the trailing 4)
        assert!(
            !result.contains("GPT-44"),
            "got double-counted result: {}",
            result
        );
    }

    #[test]
    fn test_apply_custom_words_matches_ampersand_word() {
        let text = "send it to RD for review";
        let custom_words = vec!["R&D".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.18);
        assert_eq!(result, "send it to R&D for review");
    }

    #[test]
    fn test_apply_custom_words_matches_spoken_ampersand_word() {
        let text = "send it to R and D for review";
        let custom_words = vec!["R&D".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.18);
        assert_eq!(result, "send it to R&D for review");
    }

    #[test]
    fn test_apply_custom_words_preserves_ampersand_word() {
        let text = "send it to R&D for review";
        let custom_words = vec!["R&D".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.18);
        assert_eq!(result, "send it to R&D for review");
    }

    #[test]
    fn test_apply_custom_words_handles_unicode_punctuation() {
        let text = "「Handee。」";
        let custom_words = vec!["Handy".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "「Handy。」");
    }

    #[test]
    fn test_apply_custom_words_skips_cjk_fuzzy_matching() {
        let text = "你好。";
        let custom_words = vec!["你号".to_string()];
        let result = apply_custom_words(text, &custom_words, 1.0);
        assert_eq!(result, text);
    }

    /// Dictations with a spoken command, and what is pasted for them when
    /// the paste can carry line breaks. `<NL>` stands for a line break; a
    /// line that starts with `#` is a note.
    const SPOKEN_COMMANDS: &str = r#"
# Line and paragraph breaks.
Hi John. New line. Thanks for the update. => Hi John.<NL>Thanks for the update.
Hi John, new line, thanks for the update. => Hi John,<NL>Thanks for the update.
The intro is done. New paragraph. The second part starts here. => The intro is done.<NL><NL>The second part starts here.
Line one is short. Newline. Line two is longer. => Line one is short.<NL>Line two is longer.
Dear team, new paragraph. I will be late tomorrow. => Dear team,<NL><NL>I will be late tomorrow.
New line. Hello there. => Hello there.
Hello there. New line. => Hello there.
Thanks, new line. => Thanks,
Alpha is ready. New line. New line. Beta is not. => Alpha is ready.<NL><NL>Beta is not.
Alpha is ready. New paragraph. New paragraph. Beta is not. => Alpha is ready.<NL><NL>Beta is not.
Send it to Anna. New line, new line. Best, Maryan. => Send it to Anna.<NL><NL>Best, Maryan.
Um was removed. New Line. Capital L here. => Um was removed.<NL>Capital L here.
# Bullets.
Shopping list. Bullet point milk. Bullet point eggs. Bullet point bread. => Shopping list.<NL>- Milk<NL>- Eggs<NL>- Bread
Bullet point, milk. Next bullet, eggs. => - Milk<NL>- Eggs
Bullet point milk, bullet point eggs, bullet point bread. => - Milk<NL>- Eggs<NL>- Bread
Notes for today: bullet point standup moved, bullet point demo on Friday. => Notes for today:<NL>- Standup moved<NL>- Demo on Friday
Bullet point call the bank. They close at five. Bullet point book flights. Let me know. => - Call the bank. They close at five<NL>- Book flights<NL>Let me know.
To do. Next bullet point call mom. Next bullet point pay rent. => To do.<NL>- Call mom<NL>- Pay rent
Bullet point are we shipping Friday? Bullet point who is on call? => - Are we shipping Friday?<NL>- Who is on call?
# Numbered lists.
Here is the plan. Number one, fix the bug. Number two, ship it. Let me know if that works. => Here is the plan.<NL>1. Fix the bug<NL>2. Ship it<NL>Let me know if that works.
Number 1, apples. Number 2, pears. Number 3, milk. => 1. Apples<NL>2. Pears<NL>3. Milk
Number one. Fix the login bug. Number two. Ship it on Friday. => 1. Fix the login bug<NL>2. Ship it on Friday
One, it is too expensive. Two, nobody asked for it. => 1. It is too expensive<NL>2. Nobody asked for it
Three reasons. One, speed. Two, cost. Three, it is local. => Three reasons.<NL>1. Speed<NL>2. Cost<NL>3. It is local
Number one, apples. Two, pears. Three, milk. => 1. Apples<NL>2. Pears<NL>3. Milk
Agenda: one, budget. Two, hiring. => Agenda:<NL>1. Budget<NL>2. Hiring
Number one, apples. Number two, pears. New paragraph. That is all. => 1. Apples<NL>2. Pears<NL><NL>That is all.
We ship on Friday. Number one, tests pass, number two, docs are done. => We ship on Friday.<NL>1. Tests pass<NL>2. Docs are done
Hello. New line. Number one, write tests. Number two, ship. => Hello.<NL>1. Write tests<NL>2. Ship
Number one, apples. Number two, pears. Number two, plums. => 1. Apples<NL>2. Pears<NL>Number two, plums.
Bullet point. New line. Done. => Bullet point.<NL>Done.
# Known false positives, pinned so they are a decision.
How many do you want? One. And for her? Two. Okay. => How many do you want?<NL>1. And for her?<NL>2. Okay
# What Parakeet Ultra wrote for generated speech (six voices, 2026-11-16)
# where it differs from the forms above: a comma for a short pause,
# "Newline" as one word, digits for the numbers of a list.
Hi John, Newline, thanks for the update. => Hi John,<NL>Thanks for the update.
Hi John Newline, thanks for the update. => Hi John<NL>Thanks for the update.
Hello there, new line. => Hello there,
New line, hello there. => Hello there.
Line 1 is short, new line, line 2 is longer. => Line 1 is short,<NL>Line 2 is longer.
Alpha is ready, new line, new line, beta is not. => Alpha is ready,<NL><NL>Beta is not.
See you on Mundy, New Line, Best Regards, New Line, Marion. => See you on Mundy,<NL>Best Regards,<NL>Marion.
The intro is done, new paragraph, the second part starts here. => The intro is done,<NL><NL>The second part starts here.
Dear team, New Paragraph, I will be late tomorrow. => Dear team,<NL><NL>I will be late tomorrow.
Dear team. New paragraph. I will be late tomorrow. New line. Best, Marion. => Dear team.<NL><NL>I will be late tomorrow.<NL>Best, Marion.
Shopping list, bullet point milk, bullet point eggs, bullet point bread. => Shopping list,<NL>- Milk<NL>- Eggs<NL>- Bread
Notes for today, bullet point stand-up moved, bullet point demo on Friday. => Notes for today,<NL>- Stand-up moved<NL>- Demo on Friday
Here is what we need, bullet point a new laptop, bullet point a second monitor, bullet point a desk. => Here is what we need,<NL>- A new laptop<NL>- A second monitor<NL>- A desk
# A number right after "bullet point" names a bullet, so that one stays in
# the item before it.
Here is what we need, bullet point a new laptop, bullet point two monitors, bullet point a desk. => Here is what we need,<NL>- A new laptop, bullet point two monitors<NL>- A desk
Bullet point, 2 monitors. Bullet point, a desk. => - 2 monitors<NL>- A desk
Bullet point, milk, next bullet, eggs, new bullet, bread. => - Milk<NL>- Eggs<NL>- Bread
Bullet point call the bank, they close at five, bullet point book flights, let me know. => - Call the bank, they close at five<NL>- Book flights, let me know
Next bullet, call the bank. Next bullet, book the flights. => - Call the bank<NL>- Book the flights
Number 1, apples. Number 2, pears. => 1. Apples<NL>2. Pears
Number one, apples, number two, pears. => 1. Apples<NL>2. Pears
Here is the plan, number one, fix the bug, number two, ship it, let me know if that works. => Here is the plan,<NL>1. Fix the bug<NL>2. Ship it, let me know if that works
My priorities are, number one, speed, number two, cost. => My priorities are,<NL>1. Speed<NL>2. Cost
Number 1. Fix the login bug. Number 2. Ship it on Friday. => 1. Fix the login bug<NL>2. Ship it on Friday
Number 1, apples, number 2, pears, number 3, milk, new paragraph, that is all. => 1. Apples<NL>2. Pears<NL>3. Milk<NL><NL>That is all.
Number 1, call the bank. Number 2, book the flights, number 3, pack. => 1. Call the bank<NL>2. Book the flights<NL>3. Pack
# A count inside an item is not the next item.
Number one, sizes one, two, and three. Number two, colours. => 1. Sizes one, two, and three<NL>2. Colours
Number 1, sizes 1, 2, and 3. Number 2, colours. => 1. Sizes 1, 2, and 3<NL>2. Colours
Number one, we need three, four, maybe five servers. Number two, ship. => 1. We need three, four, maybe five servers<NL>2. Ship
1. Speed. 2. Cost. => 1. Speed<NL>2. Cost
Three reasons. 1. It is too expensive. 2. Nobody asked for it. 3. It is local. => Three reasons.<NL>1. It is too expensive<NL>2. Nobody asked for it<NL>3. It is local
I have two points. 1. The price is too high. 2. The delivery is too slow. That is it. => I have two points.<NL>1. The price is too high<NL>2. The delivery is too slow<NL>That is it.
I have two points. 1. the price is too high, 2. the delivery is too slow, that is it. => I have two points.<NL>1. The price is too high<NL>2. The delivery is too slow, that is it
# Observed and only half understood: the later markers have no punctuation
# before them, so they stay in the item.
Number 1, call the bank, number 2, book the flights, number 3 pack. => 1. Call the bank<NL>2. Book the flights, number 3 pack
"#;

    /// Dictations that come back byte for byte.
    const SPOKEN_UNCHANGED: &str = r#"
We need a new line of products for the spring.
Start a new paragraph in the document.
This is a new line.
New line items were added to the invoice.
I said new line, not new paragraph.
Our number one priority is speed.
Number one priority is speed.
Number one is done. Number two is next.
Number two, pears. Number three, milk.
It was number one, number two was worse.
Number one, I think, number three, is fine.
First of all, thanks. The second thing I noticed is the price.
First, we went to the store. Second, we went home.
HiveMind builds it as your first feed. Two new brains keep you company.
One of the things I noticed is the price. Two of them are broken.
Pick a number. One, two or three.
One, two, three, go.
One. Two. Three. Go.
One, I agree. Three, I do not.
The answers can now be up to 2,000 characters.
Bullet points are useful.
He dodged a bullet point blank.
# One "bullet point" is not a list: a list has two.
Bullet point buy milk.
Bullet point milk bullet point eggs.
Bullet point formatting works well in this doc.
I like it. Bullet point formatting works well in this doc.
Next bullet train leaves at five.
New bullet points were added to the deck.
Next bullet point is about pricing.
Bullet Point drei ist falsch.
# "Bullet point" with a number names a bullet.
Bullet point three is wrong.
Bullet point 3 is wrong.
Bullet point two needs rewording.
Bullet point three on the slide is wrong, bullet point four is fine.
On slide five, bullet point two is wrong, bullet point three is fine.
Das ist eine neue Zeile. Erstens gehen wir.
# Known miss: no comma after the numeral.
Number one apples. Number two pears.
# Nothing but commands: the words are pasted, not nothing.
New line.
New line
New paragraph
New line. New paragraph.
# Ordinary sentences as Parakeet Ultra wrote them.
Print a new line, then the name.
Add a new line at the end of the file.
The function should end with a new line.
The first bullet point is wrong and the second bullet point is too long.
Pick a number, 1, 2 or 3.
1, 2, 3, go.
1. I agree. 3. I do not.
One apple, two pears and three bananas.
The score was 3-1. Two players were injured.
Line 1 is short. Line 2 is longer.
# Known misses, as Parakeet Ultra wrote them: a command spoken without a
# pause has no punctuation around it.
Thanks for the call new line I will send the notes tomorrow.
That is the first point new paragraph the second point is cost.
Dear team New paragraph I will be late to morrow New line Best Marion
Shopping list bullet point Milk Bullet point Eggs Bullet point Bread
Number 1 apples number 2 pears number 3 milk
Number one Call the bank. Number two Book the flights.
My priorities are number 1, speed, number 2, cost.
I have two points, 1, the price is too high, 2, the delivery is too slow, that is it.
Agenda 1, Budget 2, hiring.
1 speed 2 cost
1, speed, 2, cost.
1. Speed, 2, cost.
"#;

    /// What is typed when the paste cannot carry line breaks.
    const SPOKEN_SINGLE_LINE: &str = r#"
Hi John. New line. Thanks for the update. => Hi John. Thanks for the update.
Dear team, new paragraph. I will be late tomorrow. => Dear team, I will be late tomorrow.
Alpha is ready. New line. New line. Beta is not. => Alpha is ready. Beta is not.
Number one, apples. Number two, pears. Let me know. => 1. Apples; 2. Pears; Let me know.
Shopping list. Bullet point milk. Bullet point eggs. => Shopping list. - Milk; - Eggs
Hi John Newline, thanks for the update. => Hi John; Thanks for the update.
"#;

    fn spoken_rows(table: &'static str) -> impl Iterator<Item = &'static str> {
        table
            .lines()
            .filter(|row| !row.is_empty() && !row.starts_with('#'))
    }

    fn spoken_cases(table: &'static str) -> impl Iterator<Item = (&'static str, String)> {
        spoken_rows(table).map(|row| {
            let (input, expected) = row.split_once(" => ").expect("input => output");
            (input, expected.replace("<NL>", "\n"))
        })
    }

    #[test]
    fn test_format_spoken_commands() {
        for (input, expected) in spoken_cases(SPOKEN_COMMANDS) {
            assert_eq!(format_spoken(input, true), expected, "input: {input}");
        }
    }

    #[test]
    fn test_format_spoken_leaves_ordinary_text_alone() {
        for input in spoken_rows(SPOKEN_UNCHANGED).chain([""]) {
            assert_eq!(format_spoken(input, true), input);
            assert_eq!(format_spoken(input, false), input);
        }
        // Text that already has line breaks is not touched either.
        let lines = "Hi John.\nNew line. Thanks.";
        assert_eq!(format_spoken(lines, true), lines);
        // Spacing survives when there is no command.
        let spaced = " We need a  new line of products. ";
        assert_eq!(format_spoken(spaced, true), spaced);
    }

    #[test]
    fn test_format_spoken_single_line() {
        for (input, expected) in spoken_cases(SPOKEN_SINGLE_LINE) {
            assert_eq!(format_spoken(input, false), expected, "input: {input}");
        }
    }

    #[test]
    fn test_format_spoken_output_shape() {
        let inputs = spoken_cases(SPOKEN_COMMANDS)
            .chain(spoken_cases(SPOKEN_SINGLE_LINE))
            .map(|(input, _)| input)
            .chain(spoken_rows(SPOKEN_UNCHANGED));
        let mut count = 0;
        for input in inputs {
            count += 1;
            let lines = format_spoken(input, true);
            assert_eq!(lines, lines.trim(), "input: {input}");
            assert!(!lines.is_empty(), "input: {input}");
            assert!(!lines.contains("\n\n\n"), "input: {input}");
            assert!(!lines.contains(" \n"), "input: {input}");

            let line = format_spoken(input, false);
            assert_eq!(line, line.trim(), "input: {input}");
            assert!(!line.contains('\n'), "input: {input}");
            // The same input always gives the same output.
            assert_eq!(format_spoken(input, true), lines);
        }
        assert!(count > 100, "the tables were read: {count} rows");
    }

    #[test]
    fn test_format_spoken_survives_odd_input() {
        for input in [
            "…",
            "... New line. ...",
            "Bullet point … .",
            "Number one, . Number two, !",
            "New line. 你好。 Bullet point 牛奶。",
            "Number 1, ß. Number 2, éa.",
            "new\u{a0}line.",
        ] {
            let _ = format_spoken(input, true);
            let _ = format_spoken(input, false);
        }
    }
}
