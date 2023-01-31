#![no_std]
#![no_main]

use core::cell::RefCell;
use critical_section::Mutex;

use esp32c3_hal::{
    interrupt,
    clock::{ClockControl, CpuClock},
    gpio::{Event, Floating, Gpio19, Input, Pin},
    prelude::*,
    system::SystemExt,
    systimer::SystemTimer,
    Rng, Rtc, peripherals::{self, Peripherals},
};

/*
use esp32c3_hal::{
    clock::{CpuClock, ClockControl},
    interrupt,
    peripherals::{self, Peripherals, TIMG0, TIMG1},
    prelude::*,
    timer::{Timer, Timer0, TimerGroup},
    Rtc,
};
 */

use riscv_rt::{entry};

use embedded_svc::{
    ipv4::Interface,
    wifi::{ClientConfiguration, Configuration, Wifi},
};


use esp_backtrace as _;
use esp_println::{logger::init_logger, println};

use esp_wifi::{
    create_network_stack_storage, network_stack_storage, wifi::utils::create_network_interface,
    wifi_interface::Network,
};

use heapless::mpmc::MpMcQueue;

use infrared::{
    remotecontrol::{Action, Button},
    Receiver,
};
use infrared::protocol::SamsungNec;
use infrared::remotecontrol::nec::SamsungTv;
use mqttrust::{encoding::v4::Pid, MqttError};
use smoltcp::wire::Ipv4Address;


use crate::{pins::Pins, tiny_mqtt::TinyMqtt};

mod pins;
mod tiny_mqtt;

type IrPin = Gpio19<Input<Floating>>;
type IrReceiver = Receiver<SamsungNec, IrPin, u64, Button<SamsungTv>>;

static IR_RECV: Mutex<RefCell<Option<IrReceiver>>> = Mutex::new(RefCell::new(None));
static IR_EVENTS: MpMcQueue<Action, 8> = MpMcQueue::new();

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASS");
const MQTT_HOST: &str = env!("MQTT_HOST");
const MQTT_TOPIC: &str = env!("MQTT_TOPIC");
const MQTT_HOST_BYTES: [u8; 4] = mqtt_host_bytes();

#[interrupt]
fn GPIO() {
    let t = current_micros();

    critical_section::with(|cs| {
        let mut ir_recv = IR_RECV.borrow_ref_mut(cs);

        if let Some(recv) = ir_recv.as_mut() {
            match recv.event_instant(t) {
                Ok(Some(cmd)) => {
                    println!("Cmd: {:?}", cmd);
                    if let Some(action) = cmd.action() {
                        IR_EVENTS.enqueue(action).unwrap();
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    println!("Recv err: {:?}", err);
                }
            };

            recv.pin_mut().clear_interrupt();
        }
    });
}


#[entry]
fn main() -> ! {
    init_logger(log::LevelFilter::Info);
    esp_wifi::init_heap();

    let peripherals = Peripherals::take();
    let system = peripherals.SYSTEM.split();
    let clocks = ClockControl::configure(system.clock_control, CpuClock::Clock160MHz).freeze();

    let mut rtc = Rtc::new(peripherals.RTC_CNTL);

    // Disable watchdog timers
    rtc.swd.disable();
    rtc.rwdt.disable();

    // Pins
    let pins = Pins::new(peripherals.GPIO, peripherals.IO_MUX);

    let mut led = pins.led.into_push_pull_output();
    let mut ir = pins.io19.into_floating_input();
    ir.listen(Event::AnyEdge);

    let ir_recv = infrared::Receiver::builder()
        .pin(ir)
        .nec_samsung()
        .remotecontrol(SamsungTv::default())
        .monotonic::<u64>()
        .build();

    critical_section::with(|cs| IR_RECV.borrow_ref_mut(cs).replace(ir_recv));

    interrupt::enable(
        peripherals::Interrupt::GPIO,
        interrupt::Priority::Priority3,
    )
    .unwrap();

    unsafe {
        riscv::interrupt::enable();
    }


    let mut storage = create_network_stack_storage!(3, 8, 1, 1);
    let ethernet = create_network_interface(network_stack_storage!(storage));
    let mut wifi_interface = esp_wifi::wifi_interface::Wifi::new(ethernet);

    let rng = Rng::new(peripherals.RNG);
    let syst = SystemTimer::new(peripherals.SYSTIMER);

    let r = esp_wifi::initialize(syst.alarm0, rng, &clocks);

    if let Err(e) = r {
        println!("Err: {:?}", e);
        loop {}
    }

    println!("Wifi started: {:?}", wifi_interface.is_started());

    let host_bytes: [u8; 4] = MQTT_HOST_BYTES;

    println!("Call wifi_connect");
    let client_config = Configuration::Client(ClientConfiguration {
        ssid: SSID.into(),
        password: PASSWORD.into(),
        ..Default::default()
    });
    let res = wifi_interface.set_configuration(&client_config);
    println!("wifi_connect returned {:?}", res);
    println!("  Cap   : {:?}", wifi_interface.get_capabilities());

    led.set_high().unwrap();

    wifi_interface.connect().unwrap();

    // wait to get connected
    loop {
        let connected = wifi_interface.is_connected();

        match connected {
            Ok(true) => break,
            Ok(false) => continue,
            Err(err) => {
                println!("Error connecting: {:?}", err);
                loop {}
            }
        }
    }
    println!("Connected: {:?}", wifi_interface.is_connected());
    println!("Starting dhcp client");

    let network = Network::new(wifi_interface, current_millis);

    loop {
        network.poll_dhcp().unwrap();
        network.work();
        if network.is_iface_up() {
            break;
        }
    }

    println!("Got ip: {:?}", network.get_ip_info());
    println!("Trying to connect to: {:?}", host_bytes);

    let mut rx_buffer = [0u8; 1536];
    let mut tx_buffer = [0u8; 1536];
    let mut socket = network.get_socket(&mut rx_buffer, &mut tx_buffer);
    let mqtt_host = Ipv4Address::from_bytes(&host_bytes);
    socket.open(mqtt_host, 1883).unwrap();
    let mut mqtt = TinyMqtt::new("esp32", socket, esp_wifi::current_millis, None);

    println!("Start busy loop on main");
    loop {
        sleep_millis(1_000);

        println!("Trying to connect");
        mqtt.disconnect().ok();

        if let Err(e) = mqtt.connect(mqtt_host, 1883, 10, None, None) {
            println!(
                "Something went wrong ... retrying in 10 seconds. Error is {:?}",
                e
            );
            // wait a bit and try it again
            sleep_millis(10_000);
            continue;
        }

        println!("Connected to MQTT broker");

        let mut pkt_num = 10;
        loop {
            if mqtt.poll().is_err() {
                break;
            }

            if let Some(action) = IR_EVENTS.dequeue() {
                if let Err(err) = publish_cmd(&mqtt, MQTT_TOPIC, pkt_num, action) {
                    println!("Mqtt err: {:?}", err);
                }
                pkt_num += 1;
            }
        }

        println!("Disconnecting");
        mqtt.disconnect().ok();
    }

}

fn publish_cmd(
    mqtt: &TinyMqtt,
    topic: &str,
    pkt_num: u16,
    action: Action,
) -> Result<(), MqttError> {
    let msg = action.to_str();
    mqtt.publish_with_pid(
        Pid::try_from(pkt_num).ok(),
        topic,
        msg.as_bytes(),
        mqttrust::QoS::AtLeastOnce,
    )
}

pub fn current_millis() -> u64 {
    esp_wifi::timer::get_systimer_count() * 1_000 / esp_wifi::timer::TICKS_PER_SECOND
}

pub fn current_micros() -> u64 {
    esp_wifi::timer::get_systimer_count() / (esp_wifi::timer::TICKS_PER_SECOND / 1_000_000)
}

pub fn sleep_millis(delay: u32) {
    let sleep_end = current_millis() + delay as u64;
    while current_millis() < sleep_end {
        // wait
    }
}

const fn mqtt_host_bytes() -> [u8; 4] {
    let parts: [&str; 4] = const_str::split!(MQTT_HOST, '.');
    let mut res: [u8; 4] = [0; 4];
    let mut i = 0;
    while i < 4 {
        res[i] = const_str::parse!(parts[i], u8);
        i += 1;
    }

    res
}

