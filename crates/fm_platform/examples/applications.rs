// A read-only discovery probe uses the same provider and service checks as the desktop app.
// It supports installed-machine evidence without starting the app or changing member setup.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let installs = fm_platform::detect_applications();
    println!("{}", serde_json::to_string_pretty(&installs)?);
    Ok(())
}
