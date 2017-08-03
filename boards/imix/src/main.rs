#![no_std]
#![no_main]
#![feature(asm,const_fn,drop_types_in_const,lang_items,compiler_builtins_lib)]

extern crate capsules;
extern crate compiler_builtins;
#[macro_use(debug, static_init)]
extern crate kernel;
extern crate sam4l;

use capsules::rf233::RF233;
use capsules::timer::TimerDriver;
use capsules::virtual_alarm::{MuxAlarm, VirtualMuxAlarm};
use capsules::virtual_i2c::{I2CDevice, MuxI2C};
use capsules::virtual_spi::{VirtualSpiMasterDevice, MuxSpiMaster};
use kernel::hil;
use kernel::hil::Controller;
use kernel::hil::radio;
use kernel::hil::radio::{RadioConfig, RadioData};
use kernel::hil::spi::SpiMaster;

#[macro_use]
pub mod io;

// Unit Tests for drivers.
#[allow(dead_code)]
mod i2c_dummy;
#[allow(dead_code)]
mod spi_dummy;

#[allow(dead_code)]
mod power;

// State for loading apps.

const NUM_PROCS: usize = 2;

// how should the kernel respond when a process faults
const FAULT_RESPONSE: kernel::process::FaultResponse = kernel::process::FaultResponse::Panic;

#[link_section = ".app_memory"]
static mut APP_MEMORY: [u8; 16384] = [0; 16384];

static mut PROCESSES: [Option<kernel::Process<'static>>; NUM_PROCS] = [None, None];

// Save some deep nesting
type RF233Device = capsules::rf233::RF233<'static,
                                          VirtualSpiMasterDevice<'static, sam4l::spi::Spi>>;
struct Imix {
    console: &'static capsules::console::Console<'static, sam4l::usart::USART>,
    gpio: &'static capsules::gpio::GPIO<'static, sam4l::gpio::GPIOPin>,
    timer: &'static TimerDriver<'static, VirtualMuxAlarm<'static, sam4l::ast::Ast<'static>>>,
    si7021: &'static capsules::si7021::SI7021<'static,
                                              VirtualMuxAlarm<'static, sam4l::ast::Ast<'static>>>,
    isl29035: &'static capsules::isl29035::Isl29035<'static,
                                                    VirtualMuxAlarm<'static,
                                                                    sam4l::ast::Ast<'static>>>,
    adc: &'static capsules::adc::Adc<'static, sam4l::adc::Adc>,
    led: &'static capsules::led::LED<'static, sam4l::gpio::GPIOPin>,
    button: &'static capsules::button::Button<'static, sam4l::gpio::GPIOPin>,
    spi: &'static capsules::spi::Spi<'static, VirtualSpiMasterDevice<'static, sam4l::spi::Spi>>,
    ipc: kernel::ipc::IPC,
    ninedof: &'static capsules::ninedof::NineDof<'static>,
    radio: &'static capsules::radio::RadioDriver<'static,
                                                 capsules::mac::MacDevice<'static, RF233Device>>,
    crc: &'static capsules::crc::Crc<'static, sam4l::crccu::Crccu<'static>>,
    usb_driver: &'static capsules::usb_user::UsbSyscallDriver<'static,
                        capsules::usbc_client::Client<'static, sam4l::usbc::Usbc<'static>>>,
}

// The RF233 radio stack requires our buffers for its SPI operations:
//
//   1. buf: a packet-sized buffer for SPI operations, which is
//      used as the read buffer when it writes a packet passed to it and the write
//      buffer when it reads a packet into a buffer passed to it.
//   2. rx_buf: buffer to receive packets into
//   3 + 4: two small buffers for performing registers
//      operations (one read, one write).

static mut RF233_BUF: [u8; radio::MAX_BUF_SIZE] = [0x00; radio::MAX_BUF_SIZE];
static mut RF233_RX_BUF: [u8; radio::MAX_BUF_SIZE] = [0x00; radio::MAX_BUF_SIZE];
static mut RF233_REG_WRITE: [u8; 2] = [0x00; 2];
static mut RF233_REG_READ: [u8; 2] = [0x00; 2];
// The RF233 system call interface ("radio") requires one buffer, which it
// copies application transmissions into or copies out to application buffers
// for reception.
static mut RADIO_BUF: [u8; radio::MAX_BUF_SIZE] = [0x00; radio::MAX_BUF_SIZE];

impl kernel::Platform for Imix {
    fn with_driver<F, R>(&self, driver_num: usize, f: F) -> R
        where F: FnOnce(Option<&kernel::Driver>) -> R
    {
        match driver_num {
            0 => f(Some(self.console)),
            1 => f(Some(self.gpio)),

            3 => f(Some(self.timer)),
            4 => f(Some(self.spi)),
            6 => f(Some(self.isl29035)),
            7 => f(Some(self.adc)),
            8 => f(Some(self.led)),
            9 => f(Some(self.button)),
            10 => f(Some(self.si7021)),
            11 => f(Some(self.ninedof)),
            16 => f(Some(self.crc)),
            34 => f(Some(self.usb_driver)),
            154 => f(Some(self.radio)),
            0xff => f(Some(&self.ipc)),
            _ => f(None),
        }
    }
}

unsafe fn set_pin_primary_functions() {
    use sam4l::gpio::{PA, PB, PC};
    use sam4l::gpio::PeripheralFunction::{A, B, C, E};

    // Right column: Imix pin name
    // Left  column: SAM4L peripheral function
    PA[04].configure(Some(A)); // AD0         --  ADCIFE AD0
    PA[05].configure(Some(A)); // AD1         --  ADCIFE AD1
    PA[06].configure(Some(C)); // EXTINT1     --  EIC EXTINT1
    PA[07].configure(Some(A)); // AD1         --  ADCIFE AD2
    PA[08].configure(None); //... RF233 IRQ   --  GPIO pin
    PA[09].configure(None); //... RF233 RST   --  GPIO pin
    PA[10].configure(None); //... RF233 SLP   --  GPIO pin
    PA[13].configure(None); //... TRNG EN     --  GPIO pin
    PA[14].configure(None); //... TRNG_OUT    --  GPIO pin
    PA[17].configure(None); //... NRF INT     -- GPIO pin
    PA[18].configure(Some(A)); // NRF CLK     -- USART2_CLK
    PA[20].configure(None); //... D8          -- GPIO pin
    PA[21].configure(Some(E)); // TWI2 SDA    -- TWIM2_SDA
    PA[22].configure(Some(E)); // TWI2 SCL    --  TWIM2 TWCK
    PA[25].configure(Some(A)); // USB_N       --  USB DM
    PA[26].configure(Some(A)); // USB_P       --  USB DP
    PB[00].configure(Some(A)); // TWI1_SDA    --  TWIMS1 TWD
    PB[01].configure(Some(A)); // TWI1_SCL    --  TWIMS1 TWCK
    PB[02].configure(Some(A)); // AD3         --  ADCIFE AD3
    PB[03].configure(Some(A)); // AD4         --  ADCIFE AD4
    PB[04].configure(Some(A)); // AD5         --  ADCIFE AD5
    PB[05].configure(Some(A)); // VHIGHSAMPLE --  ADCIFE AD6
    PB[06].configure(Some(A)); // RTS3        --  USART3 RTS
    PB[07].configure(None); //... NRF RESET   --  GPIO
    PB[09].configure(Some(A)); // RX3         --  USART3 RX
    PB[10].configure(Some(A)); // TX3         --  USART3 TX
    PB[11].configure(Some(A)); // CTS0        --  USART0 CTS
    PB[12].configure(Some(A)); // RTS0        --  USART0 RTS
    PB[13].configure(Some(A)); // CLK0        --  USART0 CLK
    PB[14].configure(Some(A)); // RX0         --  USART0 RX
    PB[15].configure(Some(A)); // TX0         --  USART0 TX
    PC[00].configure(Some(A)); // CS2         --  SPI NPCS2
    PC[01].configure(Some(A)); // CS3 (RF233) --  SPI NPCS3
    PC[02].configure(Some(A)); // CS1         --  SPI NPCS1
    PC[03].configure(Some(A)); // CS0         --  SPI NPCS0
    PC[04].configure(Some(A)); // MISO        --  SPI MISO
    PC[05].configure(Some(A)); // MOSI        --  SPI MOSI
    PC[06].configure(Some(A)); // SCK         --  SPI CLK
    PC[07].configure(Some(B)); // RTS2 (BLE)  -- USART2_RTS
    PC[08].configure(Some(E)); // CTS2 (BLE)  -- USART2_CTS
    PC[09].configure(None); //... NRF GPIO    -- GPIO
    PC[10].configure(None); //... USER LED    -- GPIO
    PC[11].configure(Some(B)); // RX2 (BLE)   -- USART2_RX
    PC[12].configure(Some(B)); // TX2 (BLE)   -- USART2_TX
    PC[13].configure(None); //... ACC_INT1    -- GPIO
    PC[14].configure(None); //... ACC_INT2    -- GPIO
    PC[16].configure(None); //... SENSE_PWR   --  GPIO pin
    PC[17].configure(None); //... NRF_PWR     --  GPIO pin
    PC[18].configure(None); //... RF233_PWR   --  GPIO pin
    PC[19].configure(None); //... TRNG_PWR    -- GPIO Pin
    PC[22].configure(None); //... KERNEL LED  -- GPIO Pin
    PC[24].configure(None); //... USER_BTN    -- GPIO Pin
    PC[25].configure(Some(B)); // LI_INT      --  EIC EXTINT2
    PC[26].configure(None); //... D7          -- GPIO Pin
    PC[27].configure(None); //... D6          -- GPIO Pin
    PC[28].configure(None); //... D5          -- GPIO Pin
    PC[29].configure(None); //... D4          -- GPIO Pin
    PC[30].configure(None); //... D3          -- GPIO Pin
    PC[31].configure(None); //... D2          -- GPIO Pin
}

#[no_mangle]
pub unsafe fn reset_handler() {
    sam4l::init();

    sam4l::pm::PM.setup_system_clock(sam4l::pm::SystemClockSource::PllExternalOscillatorAt48MHz {
        frequency: sam4l::pm::OscillatorFrequency::Frequency16MHz,
        startup_mode: sam4l::pm::OscillatorStartup::FastStart,
    });

    // Source 32Khz and 1Khz clocks from RC23K (SAM4L Datasheet 11.6.8)
    sam4l::bpm::set_ck32source(sam4l::bpm::CK32Source::RC32K);

    set_pin_primary_functions();

    power::configure_submodules(power::SubmoduleConfig {
        rf233: true,
        nrf51422: true,
        sensors: true,
        trng: true,
    });

    // # CONSOLE

    let console = static_init!(
        capsules::console::Console<sam4l::usart::USART>,
        capsules::console::Console::new(&sam4l::usart::USART3,
                     115200,
                     &mut capsules::console::WRITE_BUF,
                     kernel::Container::create()));
    hil::uart::UART::set_client(&sam4l::usart::USART3, console);
    console.initialize();

    // Attach the kernel debug interface to this console
    let kc = static_init!(
        capsules::console::App,
        capsules::console::App::default());
    kernel::debug::assign_console_driver(Some(console), kc);

    // # TIMER

    let ast = &sam4l::ast::AST;

    let mux_alarm = static_init!(
        MuxAlarm<'static, sam4l::ast::Ast>,
        MuxAlarm::new(&sam4l::ast::AST));
    ast.configure(mux_alarm);

    let virtual_alarm1 = static_init!(
        VirtualMuxAlarm<'static, sam4l::ast::Ast>,
        VirtualMuxAlarm::new(mux_alarm));
    let timer = static_init!(
        TimerDriver<'static, VirtualMuxAlarm<'static, sam4l::ast::Ast>>,
        TimerDriver::new(virtual_alarm1, kernel::Container::create()));
    virtual_alarm1.set_client(timer);

    // # I2C Sensors

    let mux_i2c = static_init!(MuxI2C<'static>, MuxI2C::new(&sam4l::i2c::I2C2));
    sam4l::i2c::I2C2.set_master_client(mux_i2c);

    // Configure the ISL29035, device address 0x44
    let isl29035_i2c = static_init!(I2CDevice, I2CDevice::new(mux_i2c, 0x44));
    let isl29035_virtual_alarm = static_init!(
        VirtualMuxAlarm<'static, sam4l::ast::Ast>,
        VirtualMuxAlarm::new(mux_alarm));
    let isl29035 = static_init!(
        capsules::isl29035::Isl29035<'static, VirtualMuxAlarm<'static, sam4l::ast::Ast>>,
        capsules::isl29035::Isl29035::new(
            isl29035_i2c,
            isl29035_virtual_alarm,
            &mut capsules::isl29035::BUF));
    isl29035_i2c.set_client(isl29035);
    isl29035_virtual_alarm.set_client(isl29035);

    // Set up an SPI MUX, so there can be multiple clients
    let mux_spi = static_init!(
        MuxSpiMaster<'static, sam4l::spi::Spi>,
        MuxSpiMaster::new(&sam4l::spi::SPI));
    sam4l::spi::SPI.set_client(mux_spi);
    sam4l::spi::SPI.init();
    sam4l::spi::SPI.enable();

    // Create a virtualized client for SPI system call interface,
    // then the system call capsule
    let syscall_spi_device = static_init!(
        VirtualSpiMasterDevice<'static, sam4l::spi::Spi>,
        VirtualSpiMasterDevice::new(mux_spi, 3));

    // Create the SPI systemc call capsule, passing the client
    let spi_syscalls = static_init!(
        capsules::spi::Spi<'static, VirtualSpiMasterDevice<'static, sam4l::spi::Spi>>,
        capsules::spi::Spi::new(syscall_spi_device));

    // System call capsule requires static buffers so it can
    // copy from application slices to DMA
    static mut SPI_READ_BUF: [u8; 64] = [0; 64];
    static mut SPI_WRITE_BUF: [u8; 64] = [0; 64];
    spi_syscalls.config_buffers(&mut SPI_READ_BUF, &mut SPI_WRITE_BUF);
    syscall_spi_device.set_client(spi_syscalls);


    // Configure the SI7021, device address 0x40
    let si7021_alarm = static_init!(
        VirtualMuxAlarm<'static, sam4l::ast::Ast>,
        VirtualMuxAlarm::new(mux_alarm));
    let si7021_i2c = static_init!(I2CDevice, I2CDevice::new(mux_i2c, 0x40));
    let si7021 = static_init!(
        capsules::si7021::SI7021<'static, VirtualMuxAlarm<'static, sam4l::ast::Ast<'static>>>,
        capsules::si7021::SI7021::new(si7021_i2c, si7021_alarm, &mut capsules::si7021::BUFFER));
    si7021_i2c.set_client(si7021);
    si7021_alarm.set_client(si7021);

    // Create a second virtualized SPI client, for the RF233
    let rf233_spi = static_init!(VirtualSpiMasterDevice<'static, sam4l::spi::Spi>,
                                 VirtualSpiMasterDevice::new(mux_spi, 3));
    // Create the RF233 driver, passing its pins and SPI client
    let rf233: &RF233<'static, VirtualSpiMasterDevice<'static, sam4l::spi::Spi>> =
        static_init!(RF233<'static, VirtualSpiMasterDevice<'static, sam4l::spi::Spi>>,
                             RF233::new(rf233_spi,
                                        &sam4l::gpio::PA[09],    // reset
                                        &sam4l::gpio::PA[10],    // sleep
                                        &sam4l::gpio::PA[08],    // irq
                                        &sam4l::gpio::PA[08])); //  irq_ctl
    sam4l::gpio::PA[08].set_client(rf233);

    // FXOS8700CQ accelerometer, device address 0x1e
    let fxos8700_i2c = static_init!(I2CDevice, I2CDevice::new(mux_i2c, 0x1e));
    let fxos8700 = static_init!(
        capsules::fxos8700cq::Fxos8700cq<'static>,
        capsules::fxos8700cq::Fxos8700cq::new(fxos8700_i2c,
                                               &sam4l::gpio::PC[13],
                                               &mut capsules::fxos8700cq::BUF));
    fxos8700_i2c.set_client(fxos8700);
    sam4l::gpio::PC[13].set_client(fxos8700);
    let ninedof = static_init!(
        capsules::ninedof::NineDof<'static>,
        capsules::ninedof::NineDof::new(fxos8700, kernel::Container::create()));
    hil::ninedof::NineDof::set_client(fxos8700, ninedof);

    // Clear sensors enable pin to enable sensor rail
    // sam4l::gpio::PC[16].enable_output();
    // sam4l::gpio::PC[16].clear();

    // Setup ADC
    let adc_channels = static_init!(
        [&'static sam4l::adc::AdcChannel; 6],
        [&sam4l::adc::CHANNEL_AD1, // AD0
         &sam4l::adc::CHANNEL_AD2, // AD1
         &sam4l::adc::CHANNEL_AD3, // AD2
         &sam4l::adc::CHANNEL_AD4, // AD3
         &sam4l::adc::CHANNEL_AD5, // AD4
         &sam4l::adc::CHANNEL_AD6, // AD5
        ]);
    let adc = static_init!(
        capsules::adc::Adc<'static, sam4l::adc::Adc>,
        capsules::adc::Adc::new(&mut sam4l::adc::ADC0, adc_channels,
                                &mut capsules::adc::ADC_BUFFER1,
                                &mut capsules::adc::ADC_BUFFER2,
                                &mut capsules::adc::ADC_BUFFER3));
    sam4l::adc::ADC0.set_client(adc);

    // # GPIO
    // set GPIO driver controlling remaining GPIO pins
    let gpio_pins = static_init!(
        [&'static sam4l::gpio::GPIOPin; 7],
        [&sam4l::gpio::PC[31], // P2
         &sam4l::gpio::PC[30], // P3
         &sam4l::gpio::PC[29], // P4
         &sam4l::gpio::PC[28], // P5
         &sam4l::gpio::PC[27], // P6
         &sam4l::gpio::PC[26], // P7
         &sam4l::gpio::PA[20], // P8
         ]);

    let gpio = static_init!(
        capsules::gpio::GPIO<'static, sam4l::gpio::GPIOPin>,
        capsules::gpio::GPIO::new(gpio_pins));
    for pin in gpio_pins.iter() {
        pin.set_client(gpio);
    }

    // # LEDs
    let led_pins = static_init!(
        [(&'static sam4l::gpio::GPIOPin, capsules::led::ActivationMode); 2],
        [(&sam4l::gpio::PC[22], capsules::led::ActivationMode::ActiveHigh),
         (&sam4l::gpio::PC[10], capsules::led::ActivationMode::ActiveHigh)
        ]);
    let led = static_init!(
        capsules::led::LED<'static, sam4l::gpio::GPIOPin>,
        capsules::led::LED::new(led_pins));

    // # BUTTONs

    let button_pins = static_init!(
        [&'static sam4l::gpio::GPIOPin; 1],
        [&sam4l::gpio::PC[24]]);

    let button = static_init!(
        capsules::button::Button<'static, sam4l::gpio::GPIOPin>,
        capsules::button::Button::new(button_pins, kernel::Container::create()));
    for btn in button_pins.iter() {
        btn.set_client(button);
    }

    let crc = static_init!(
        capsules::crc::Crc<'static, sam4l::crccu::Crccu<'static>>,
        capsules::crc::Crc::new(&mut sam4l::crccu::CRCCU, kernel::Container::create()));

    rf233_spi.set_client(rf233);
    rf233.initialize(&mut RF233_BUF, &mut RF233_REG_WRITE, &mut RF233_REG_READ);

    let radio_mac = static_init!(
        capsules::mac::MacDevice<'static, RF233Device>,
        capsules::mac::MacDevice::new(rf233),
        0);
    let radio_capsule = static_init!(
        capsules::radio::RadioDriver<'static,
                                     capsules::mac::MacDevice<'static, RF233Device>>,
        capsules::radio::RadioDriver::new(radio_mac),
        0);
    radio_capsule.config_buffer(&mut RADIO_BUF);
    radio_mac.set_transmit_client(radio_capsule);
    radio_mac.set_receive_client(radio_capsule);
    rf233.set_transmit_client(radio_mac);
    rf233.set_receive_client(radio_mac, &mut RF233_RX_BUF);
    rf233.set_config_client(radio_mac);

    // Configure the USB controller
    let usb_client = static_init!(
        capsules::usbc_client::Client<'static, sam4l::usbc::Usbc<'static>>,
        capsules::usbc_client::Client::new(&sam4l::usbc::USBC));
    sam4l::usbc::USBC.set_client(usb_client);

    // Configure the USB userspace driver
    let usb_driver = static_init!(
        capsules::usb_user::UsbSyscallDriver<'static,
            capsules::usbc_client::Client<'static, sam4l::usbc::Usbc<'static>>>,
        capsules::usb_user::UsbSyscallDriver::new(
            usb_client, kernel::Container::create()));

    let imix = Imix {
        console: console,
        timer: timer,
        gpio: gpio,
        si7021: si7021,
        isl29035: isl29035,
        adc: adc,
        led: led,
        button: button,
        crc: crc,
        spi: spi_syscalls,
        ipc: kernel::ipc::IPC::new(),
        ninedof: ninedof,
        radio: radio_capsule,
        usb_driver: usb_driver,
    };

    let mut chip = sam4l::chip::Sam4l::new();

    rf233.reset();
    rf233.set_pan(0xABCD);
    rf233.set_address(0x1008);
    //    rf233.config_commit();

    rf233.start();

    debug!("Initialization complete. Entering main loop");
    extern "C" {
        /// Beginning of the ROM region containing app images.
        static _sapps: u8;
    }
    kernel::process::load_processes(&_sapps as *const u8,
                                    &mut APP_MEMORY,
                                    &mut PROCESSES,
                                    FAULT_RESPONSE);
    kernel::main(&imix, &mut chip, &mut PROCESSES, &imix.ipc);
}
