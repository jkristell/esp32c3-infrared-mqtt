
const MQTT_HOST: &str = option_env!("MQTT_HOST").unwrap_or("192.168.0.100");
const SSID: &str = env!("WIFI_SSID").unwrap_or("wikileek");
const PASSWORD: &str = env!("WIFI_PASS");
const MQTT_TOPIC: &str = option_env!("MQTT_TOPIC").unwrap_or("l");
