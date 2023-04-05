#![no_std]
#![no_main]

use core::cell::RefCell;

use critical_section::Mutex;
use embedded_svc::{
    ipv4::Interface,
    wifi::AccessPointInfo,
    wifi::{ClientConfiguration, Configuration, Wifi},
};
use esp32c3_hal::{
    clock::{ClockControl, CpuClock},
    gpio::{Event, Floating, Gpio19, Input, Pin},
    interrupt,
    peripherals::{self, Peripherals},
    prelude::*,
    riscv,
    system::SystemExt,
    systimer::SystemTimer,
    Rng, Rtc,
};
use esp_backtrace as _;
use esp_println::{logger::init_logger, println};
use esp_wifi::{
    wifi::{utils::create_network_interface, WifiError, WifiMode},
    wifi_interface::WifiStack,
};
use heapless::mpmc::MpMcQueue;
use infrared::{
    protocol::SamsungNec,
    remotecontrol::{nec::SamsungTv, Action, Button},
    Receiver,
};
use mqttrust::{encoding::v4::Pid, MqttError};
use smoltcp::{iface::SocketStorage, wire::Ipv4Address};

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

    interrupt::enable(peripherals::Interrupt::GPIO, interrupt::Priority::Priority3).unwrap();

    unsafe {
        riscv::interrupt::enable();
    }
let (wifi, _) = peripherals.RADIO.split();
    let mut socket_set_entries: [SocketStorage; 3] = Default::default();
    let (iface, device, mut controller, sockets) =
        create_network_interface( wifi, WifiMode::Sta,&mut socket_set_entries);
    let wifi_stack = WifiStack::new(iface, device, sockets, current_millis);

    let rng = Rng::new(peripherals.RNG);
    let syst = SystemTimer::new(peripherals.SYSTIMER);

    let r = esp_wifi::initialize(syst.alarm0, rng, system.radio_clock_control, &clocks);

    if let Err(e) = r {
        println!("Err: {:?}", e);
        loop {}
    }

    let client_config = Configuration::Client(ClientConfiguration {
        ssid: SSID.into(),
        password: PASSWORD.into(),
        ..Default::default()
    });
    let res = controller.set_configuration(&client_config);
    println!("wifi_set_configuration returned {:?}", res);

    controller.start().unwrap();
    println!("is wifi started: {:?}", controller.is_started());

    println!("Start Wifi Scan");
    let res: Result<(heapless::Vec<AccessPointInfo, 10>, usize), WifiError> = controller.scan_n();
    if let Ok((res, _count)) = res {
        for ap in res {
            println!("{:?}", ap);
        }
    }

    println!("{:?}", controller.get_capabilities());
    println!("wifi_connect {:?}", controller.connect());

    // wait to get connected
    println!("Wait to get connected");
    loop {
        let res = controller.is_connected();
        match res {
            Ok(connected) => {
                if connected {
                    break;
                }
            }
            Err(err) => {
                println!("{:?}", err);
                loop {}
            }
        }
    }
    println!("{:?}", controller.is_connected());

    // wait for getting an ip address
    println!("Wait to get an ip address");
    loop {
        wifi_stack.work();

        if wifi_stack.is_iface_up() {
            println!("got ip {:?}", wifi_stack.get_ip_info());
            break;
        }
    }

    println!("Start busy loop on main");

    let host_bytes: [u8; 4] = MQTT_HOST_BYTES;
    led.set_high().unwrap();

    let mut rx_buffer = [0u8; 1536];
    let mut tx_buffer = [0u8; 1536];
    let mut socket = wifi_stack.get_socket(&mut rx_buffer, &mut tx_buffer);
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
