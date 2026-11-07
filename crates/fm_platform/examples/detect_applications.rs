// Print detected application installs through the production discovery provider.
// Timing mode measures the initial scan and the watcher poll that reuses its install list.
fn main() {
    if std::env::args().any(|a| a == "--timing") {
        let t = std::time::Instant::now();
        let mut d = fm_platform::Detector::new();
        d.poll(std::time::Duration::ZERO);
        let first = t.elapsed();
        let t = std::time::Instant::now();
        d.poll(std::time::Duration::from_secs(3600));
        println!("first poll {:?}, next poll {:?}", first, t.elapsed());
        return;
    }
    let installs = fm_platform::detect_applications();
    println!("{}", serde_json::to_string_pretty(&installs).expect("serialize"));
}
