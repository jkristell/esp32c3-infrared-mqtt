fn main() {
    let dotenv_path = dotenv::dotenv().expect("failed to find .env file");
    println!("cargo:rerun-if-changed={}", dotenv_path.display());

    let keys = ["WIFI_SSID", "WIFI_PASS", "MQTT_HOST", "MQTT_TOPIC"];

    for key in keys {
        let val = dotenv::var(key).unwrap();
        println!("cargo:rustc-env={key}={val}");
    }
}
