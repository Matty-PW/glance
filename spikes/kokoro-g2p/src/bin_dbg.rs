#[path = "fallback.rs"] mod fallback;
use misaki_rs::{G2P, Language};
fn main() {
    let lang = Language::EnglishUS;
    let with = G2P::with_fallback(lang, Some(Box::new(fallback::OovFallback::new(lang))));
    let without = G2P::new(lang);
    for line in std::env::args().skip(1) {
        let (a, toks) = without.g2p(&line).unwrap();
        let (b, _) = with.g2p(&line).unwrap();
        println!("{line}\n   base    : {a}\n   fallback: {b}\n   tokens  : {:?}", toks.iter().map(|t| format!("{}|{}", t.text, t.phonemes.clone().unwrap_or("∅".into()))).collect::<Vec<_>>());
    }
}
