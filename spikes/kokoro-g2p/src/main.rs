mod fallback;
use std::collections::HashMap;
use std::time::Instant;
use misaki_rs::{G2P, Language};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    let (dir, onnx, voice, lang, out_dir) = (&a[1], &a[2], &a[3], &a[4], &a[5]);
    let tok: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{dir}/tokenizer.json"))?)?;
    let vocab: HashMap<char, i64> = tok["model"]["vocab"].as_object().unwrap().iter().map(|(k, v)| (k.chars().next().unwrap(), v.as_i64().unwrap())).collect();
    let raw = std::fs::read(format!("{dir}/voices/{voice}.bin"))?;
    let styles: Vec<f32> = raw.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    assert_eq!(styles.len() % 256, 0);
    let language = if lang == "gb" { Language::EnglishGB } else { Language::EnglishUS };
    let g2p = G2P::with_fallback(language, Some(Box::new(fallback::OovFallback::new(language))));
    let t = Instant::now();
    let mut builder = Session::builder()?.with_optimization_level(GraphOptimizationLevel::Level3)?;
    if let Ok(n) = std::env::var("THREADS") { builder = builder.with_intra_threads(n.parse()?)?; }
    let mut session = builder.commit_from_file(format!("{dir}/{onnx}"))?;
    eprintln!("model load {:?}; inputs {:?}", t.elapsed(), session.inputs().iter().map(|i| i.name().to_string()).collect::<Vec<_>>());
    std::fs::create_dir_all(out_dir)?;
    let text = std::fs::read_to_string("sentences.txt")?;
    let (mut audio_s, mut synth_s, mut unk, mut first) = (0.0f64, 0.0f64, 0, Vec::new());
    for (n, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let t = Instant::now();
        let (phonemes, _) = g2p.g2p(line)?;
        let g2p_ms = t.elapsed().as_secs_f64() * 1000.0;
        if phonemes.contains(&g2p.unk) { unk += 1; println!("  UNK left in: {line} -> {phonemes}"); }
        let ids: Vec<i64> = phonemes.chars().filter_map(|c| vocab.get(&c).copied()).collect();
        assert!(ids.len() <= 510, "too long");
        let row = &styles[ids.len() * 256..(ids.len() + 1) * 256];
        let mut input_ids = vec![0i64]; input_ids.extend(&ids); input_ids.push(0);
        let t = Instant::now();
        let outputs = session.run(ort::inputs![
            "input_ids" => Tensor::from_array(([1usize, input_ids.len()], input_ids))?,
            "style" => Tensor::from_array(([1usize, 256], row.to_vec()))?,
            "speed" => Tensor::from_array(([1usize], vec![1.0f32]))?,
        ])?;
        let (_, samples) = outputs[0].try_extract_tensor::<f32>()?;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let secs = samples.len() as f64 / 24_000.0;
        audio_s += secs; synth_s += ms / 1000.0; first.push(ms);
        let spec = hound::WavSpec { channels: 1, sample_rate: 24_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(format!("{out_dir}/s{:02}.wav", n + 1), spec)?;
        for s in samples { w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?; }
        w.finalize()?;
        std::fs::write(format!("{out_dir}/s{:02}.txt", n + 1), line)?;
        if n < 3 || line.contains("Priya") || line.contains("Kubernetes") || line.contains("9:12") || line.contains("store.rs") {
            println!("  [{:>4.0} ms synth, {:.1} ms g2p, {:.1} s audio] {line}\n      {phonemes}", ms, g2p_ms, secs);
        }
    }
    first.sort_by(|a, b| a.total_cmp(b));
    println!("== {onnx} / {voice}: {:.1} s audio in {:.1} s = {:.1}x real time; synth per sentence median {:.0} ms (min {:.0}, max {:.0}); sentences with unknowns left: {unk}", audio_s, synth_s, audio_s / synth_s, first[first.len() / 2], first[0], first[first.len() - 1]);
    Ok(())
}
