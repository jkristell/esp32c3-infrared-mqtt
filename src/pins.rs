use esp32c3_hal::{
    gpio::{Gpio18, Gpio19, Gpio2, Gpio3, Gpio4, Gpio5, Unknown},
    peripherals::{GPIO, IO_MUX},
    IO,
};

pub struct Pins {
    /// Onboard green led
    pub led: Gpio3<Unknown>,
    pub io2: Gpio2<Unknown>,
    pub io4: Gpio4<Unknown>,
    pub io5: Gpio5<Unknown>,

    /// Connected to Grove header
    pub io18: Gpio18<Unknown>,
    pub io19: Gpio19<Unknown>,
}

impl Pins {
    pub fn new(gpio: GPIO, io_mux: IO_MUX) -> Self {
        let io = IO::new(gpio, io_mux);

        let green = io.pins.gpio3;

        Pins {
            led: green,
            io2: io.pins.gpio2,
            io4: io.pins.gpio4,
            io5: io.pins.gpio5,
            io18: io.pins.gpio18,
            io19: io.pins.gpio19,
        }
    }
}
