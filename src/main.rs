#![no_std]
#![no_main]

mod wifi;

use defmt::*;
use embassy_executor::Spawner;
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embassy_stm32::spi::{Config as SpiConfig, MODE_0, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::usart::{BufferedUart, Config as UartConfig};
use embassy_time::{Delay, Timer};
use {defmt_rtt as _, panic_probe as _};

use core::cell::RefCell;
use embedded_hal_bus::spi::RefCellDevice;
use embedded_sdmmc::{Mode, SdCard, TimeSource, VolumeIdx, VolumeManager};
use heapless::{String, Vec};

const VS_WRITE_CMD: u8 = 0x02;
const SCI_MODE: u8 = 0x00;
const SCI_CLOCKF: u8 = 0x03;
const SCI_VOL: u8 = 0x0B;

// SM_SDINEW (0x0800) | SM_CANCEL (0x0008)
const MODE_NEW_CANCEL: u16 = 0x0808;

const WIFI_SSID: &str = "G416C-2G";
const WIFI_PASS: &str = "Happy12345";

// 預設音量 (0~100)
const DEFAULT_VOL: u8 = 70;
// 音量 1 時的衰減量,單位 0.5 dB。120 = 60 dB。
// 覺得整體太小聲就調小 (例如 80),太大聲就調大。
const VOL_RANGE_UNITS: u32 = 120;

/// 0~100 轉成 VS1053 SCI_VOL 值。
/// 以 dB 線性對應 (人耳感受接近對數);0 = 完全靜音 (0xFEFE,類比輸出關閉)。
fn vol_to_reg(vol: u8) -> u16 {
    let vol = core::cmp::min(vol, 100) as u32;
    if vol == 0 {
        return 0xFEFE;
    }
    let atten = ((100 - vol) * VOL_RANGE_UNITS / 99) as u16; // vol=100 -> 0, vol=1 -> 120
    (atten << 8) | atten
}

struct DummyTime;
impl TimeSource for DummyTime {
    fn get_timestamp(&self) -> embedded_sdmmc::Timestamp {
        embedded_sdmmc::Timestamp {
            year_since_1970: 53,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

macro_rules! write_register {
    ($spi:expr, $x_cs:expr, $dreq:expr, $addr:expr, $data:expr) => {
        while $dreq.is_low() {
            Timer::after_micros(100).await;
        }
        $x_cs.set_low();
        let cmd = [
            VS_WRITE_CMD,
            $addr,
            ($data >> 8) as u8,
            ($data & 0xFF) as u8,
        ];
        $spi.borrow_mut().blocking_write(&cmd).unwrap();
        $x_cs.set_high();
        Timer::after_micros(10).await;
    };
}

macro_rules! set_spi_freq {
    ($cell:expr, $cfg:expr, $hz:expr) => {
        $cfg.frequency = Hertz($hz);
        $cell.borrow_mut().set_config(&$cfg).unwrap();
    };
}

// SCI 暫存器只能用低速寫入,寫完由呼叫端自行切回 8MHz
macro_rules! set_volume {
    ($cell:expr, $cfg:expr, $cs:expr, $dreq:expr, $vol:expr) => {
        let reg_val = vol_to_reg($vol);
        set_spi_freq!($cell, $cfg, 1_000_000);
        write_register!($cell, $cs, $dreq, SCI_VOL, reg_val);
    };
}

// 送 N 個 32 byte 的 0 (end fill byte)
macro_rules! send_zeros {
    ($cell:expr, $x_dcs:expr, $dreq:expr, $blocks:expr) => {
        let zeros = [0u8; 32];
        for _ in 0..$blocks {
            while $dreq.is_low() {
                embassy_futures::yield_now().await;
            }
            $x_dcs.set_low();
            $cell.borrow_mut().blocking_write(&zeros).unwrap();
            $x_dcs.set_high();
        }
    };
}

fn make_status(
    file: Option<&String<12>>,
    vol: u8,
    pos: u32,
    size: u32,
    files: &Vec<String<12>, 32>,
    repeat_mode: wifi::RepeatMode,
    play_mode: wifi::PlayMode,
) -> wifi::Status {
    let pct = if size > 0 {
        core::cmp::min(100, (pos as u64 * 100) / size as u64) as u8
    } else {
        0
    };
    wifi::Status {
        playing: file.is_some(),
        filename: file.cloned().unwrap_or_default(),
        volume: vol,
        progress_pct: pct,
        files: files.clone(),
        repeat_mode,
        play_mode,
    }
}

#[embassy_executor::task]
async fn wifi_runner(uart: BufferedUart<'static>, ssid: String<32>, pass: String<32>, mut alarm: wifi::Alarm) {
    let mut esp = wifi::Esp8266::new(uart);
    loop {
        if esp.connect_wifi(ssid.as_str(), pass.as_str()).await.is_ok() {
            for _ in 0..5 {
                Timer::after_secs(1).await;
                if let Some(dt) = esp.get_ntp_time("time.stdtime.gov.tw").await {
                    wifi::TIME_CHANNEL.try_send(dt).unwrap_or(());
                    break;
                }
            }
            esp.run_server(ssid.as_str(), &mut alarm).await;
        }
        error!("WiFi init failed, retry in 5s");
        Timer::after_secs(5).await;
    }
}

embassy_stm32::bind_interrupts!(struct Irqs {
    UART4 => embassy_stm32::usart::BufferedInterruptHandler<embassy_stm32::peripherals::UART4>;
});

static mut TX_BUF: [u8; 4096] = [0; 4096];
static mut RX_BUF: [u8; 4096] = [0; 4096];

fn decide_track(
    current: Option<&String<12>>,
    files: &Vec<String<12>, 32>,
    repeat_mode: wifi::RepeatMode,
    play_mode: wifi::PlayMode,
    rng_seed: &mut u32,
    direction: i32, // 1 for next, -1 for prev, 0 for natural next
) -> Option<String<12>> {
    if files.is_empty() {
        return None;
    }
    if repeat_mode == wifi::RepeatMode::RepeatOne {
        if let Some(c) = current {
            return Some(c.clone());
        }
    }
    let mut idx = 0;
    if let Some(c) = current {
        for (i, f) in files.iter().enumerate() {
            if f == c {
                idx = i;
                break;
            }
        }
    }

    if play_mode == wifi::PlayMode::Random {
        *rng_seed = next_random(rng_seed);
        idx = (*rng_seed as usize) % files.len();
        return Some(files[idx].clone());
    }

    let step = if direction < 0 { -1 } else { 1 };
    let new_idx = idx as i32 + step;

    if new_idx < 0 {
        return Some(files[files.len() - 1].clone());
    } else if new_idx >= files.len() as i32 {
        return Some(files[0].clone());
    } else {
        return Some(files[new_idx as usize].clone());
    }
}

fn next_random(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    *seed
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = embassy_stm32::Config::default();
    {
        use embassy_stm32::rcc::*;
        config.rcc.pll_src = PllSource::Hsi;
        config.rcc.pll = Some(Pll {
            prediv: PllPreDiv::Div8,
            mul: PllMul::Mul168,
            divp: Some(PllPDiv::Div2),
            divq: None,
            divr: None,
        });
        config.rcc.ahb_pre = AHBPrescaler::Div1;
        config.rcc.apb1_pre = APBPrescaler::Div4;
        config.rcc.apb2_pre = APBPrescaler::Div2;
        config.rcc.sys = Sysclk::Pll1P;
    }
    let p = embassy_stm32::init(config);
    let (mut rtc, rtc_provider) = embassy_stm32::rtc::Rtc::new(p.RTC, embassy_stm32::rtc::RtcConfig::default());
    info!("Starting System...");

    let mut uart_config = UartConfig::default();
    uart_config.baudrate = 115200;
    let tx_buf = unsafe { &mut *core::ptr::addr_of_mut!(TX_BUF) };
    let rx_buf = unsafe { &mut *core::ptr::addr_of_mut!(RX_BUF) };
    let uart4 =
        BufferedUart::new(p.UART4, p.PA0, p.PA1, Irqs, tx_buf, rx_buf, uart_config).unwrap();
    let mut spi_config = SpiConfig::default();
    spi_config.frequency = Hertz(1_000_000);
    spi_config.mode = MODE_0;
    let spi = Spi::new_blocking(p.SPI1, p.PA5, p.PA7, p.PA6, spi_config);
    let spi_cell = RefCell::new(spi);

    let sd_cs = Output::new(p.PC7, Level::High, Speed::High);
    let mut x_reset = Output::new(p.PA9, Level::Low, Speed::Low);
    let mut x_dcs = Output::new(p.PA8, Level::High, Speed::High);
    let mut x_cs = Output::new(p.PB10, Level::High, Speed::High);
    let dreq = Input::new(p.PA10, Pull::None);

    x_reset.set_low();
    Timer::after_millis(20).await;
    x_reset.set_high();

    while dreq.is_low() {
        Timer::after_millis(1).await;
    }

    let mut current_volume: u8 = DEFAULT_VOL;
    set_volume!(spi_cell, spi_config, x_cs, dreq, current_volume);
    write_register!(spi_cell, x_cs, dreq, SCI_CLOCKF, 0x9800u16);
    write_register!(spi_cell, x_cs, dreq, SCI_MODE, 0x0800u16);

    // SD 卡初始化需 <= 400kHz,完成後再回 1MHz
    set_spi_freq!(spi_cell, spi_config, 400_000);

    let sd_spi = RefCellDevice::new(&spi_cell, sd_cs, embedded_hal_bus::spi::NoDelay).unwrap();
    let block = SdCard::new(sd_spi, Delay);
    let mut volume_mgr = VolumeManager::new(block, DummyTime);

    let mut volume0 = volume_mgr.open_volume(VolumeIdx(0)).unwrap();
    let mut root_dir = volume0.open_root_dir().unwrap();

    let mut ssid = String::<32>::new();
    let mut pass = String::<32>::new();
    if let Ok(mut file) = root_dir.open_file_in_dir("WIFI.TXT", Mode::ReadOnly) {
        let mut buf = [0u8; 64];
        if let Ok(n) = file.read(&mut buf) {
            let s = core::str::from_utf8(&buf[..n]).unwrap_or("");
            let mut lines = s.lines();
            if let Some(l) = lines.next() {
                let _ = ssid.push_str(l.trim());
            }
            if let Some(l) = lines.next() {
                let _ = pass.push_str(l.trim());
            }
        }
        let _ = file.close();
    }
    if ssid.is_empty() {
        let _ = ssid.push_str(WIFI_SSID);
        let _ = pass.push_str(WIFI_PASS);
    }
    
    let mut alarm = wifi::Alarm { enabled: false, hour: 0, minute: 0, days: 0 };
    if let Ok(mut file) = root_dir.open_file_in_dir("ALARM.TXT", Mode::ReadOnly) {
        let mut buf = [0u8; 32];
        if let Ok(n) = file.read(&mut buf) {
            let s = core::str::from_utf8(&buf[..n]).unwrap_or("");
            let mut lines = s.lines();
            if let Some(l) = lines.next() { alarm.enabled = l.trim() == "1"; }
            if let Some(l) = lines.next() { alarm.hour = l.trim().parse().unwrap_or(0); }
            if let Some(l) = lines.next() { alarm.minute = l.trim().parse().unwrap_or(0); }
            if let Some(l) = lines.next() { alarm.days = l.trim().parse().unwrap_or(0); }
        }
        let _ = file.close();
    }
    
    spawner.spawn(wifi_runner(uart4, ssid, pass, alarm).unwrap());

    // 只收錄 .MP3 檔案
    let mut files_cache = Vec::<String<12>, 32>::new();
    let _ = root_dir.iterate_dir(|ent| {
        if !ent.attributes.is_directory() {
            let mut name = String::<12>::new();
            if core::fmt::write(&mut name, format_args!("{}", ent.name)).is_ok()
                && name.as_str().ends_with(".MP3")
            {
                let _ = files_cache.push(name);
            }
        }
    });

    files_cache.sort_unstable();

    set_spi_freq!(spi_cell, spi_config, 1_000_000);
    info!("Ready for commands. {} files", files_cache.len());

    // 播放中收到的「換歌」指令放這裡,播完目前這首後再處理
    let mut pending: Option<wifi::PlayerCommand> = None;

    let mut repeat_mode = wifi::RepeatMode::Normal;
    let mut play_mode = wifi::PlayMode::Sequential;
    let mut last_played: Option<String<12>> = None;
    let mut _current_file_idx: Option<usize> = None;
    let mut rng_seed: u32 = 12345;
    let mut last_checked_minute: u8 = 60;
    let mut normal_play_count: u32 = 0;

    loop {
        let cmd = match pending.take() {
            Some(c) => c,
            None => {
                match embassy_time::with_timeout(embassy_time::Duration::from_secs(1), wifi::CMD_CHANNEL.receive()).await {
                    Ok(c) => c,
                    Err(_) => wifi::PlayerCommand::CheckAlarm,
                }
            }
        };

        match cmd {
            wifi::PlayerCommand::SaveAlarm(new_alarm) => {
                alarm = new_alarm;
                if let Ok(mut file) = root_dir.open_file_in_dir("ALARM.TXT", Mode::ReadWriteCreateOrTruncate) {
                    let mut buf = String::<32>::new();
                    let _ = core::fmt::write(&mut buf, format_args!("{}\n{}\n{}\n{}\n", if alarm.enabled{1}else{0}, alarm.hour, alarm.minute, alarm.days));
                    let _ = file.write(buf.as_bytes());
                    let _ = file.close();
                }
            }
            wifi::PlayerCommand::CheckAlarm => {
                while let Ok(dt) = wifi::TIME_CHANNEL.try_receive() {
                    let _ = rtc.set_datetime(dt);
                }
                
                if let Ok(dt) = rtc_provider.now() {
                    use chrono::{Datelike, Timelike, Weekday};
                    let dt_chrono: chrono::NaiveDateTime = dt.into();
                    if dt_chrono.minute() != last_checked_minute as u32 {
                        last_checked_minute = dt_chrono.minute() as u8;
                        if alarm.enabled && alarm.hour as u32 == dt_chrono.hour() && alarm.minute as u32 == dt_chrono.minute() {
                            let dow_bit = match dt_chrono.weekday() {
                                Weekday::Mon => 1, Weekday::Tue => 2, Weekday::Wed => 4,
                                Weekday::Thu => 8, Weekday::Fri => 16, Weekday::Sat => 32,
                                Weekday::Sun => 64,
                            };
                            if (alarm.days & dow_bit) != 0 {
                                play_mode = wifi::PlayMode::Random;
                                repeat_mode = wifi::RepeatMode::Normal;
                                normal_play_count = 0;
                                
                                if let Some(f) = decide_track(
                                    last_played.as_ref(),
                                    &files_cache,
                                    repeat_mode,
                                    play_mode,
                                    &mut rng_seed,
                                    1,
                                ) {
                                    pending = Some(wifi::PlayerCommand::Play(f));
                                }
                            }
                        }
                    }
                }
            }
            wifi::PlayerCommand::GetStatus => {
                rng_seed = next_random(&mut rng_seed);
                let st = make_status(
                    None,
                    current_volume,
                    0,
                    0,
                    &files_cache,
                    repeat_mode,
                    play_mode,
                );
                let _ = wifi::RESP_CHANNEL.try_send(wifi::PlayerResponse::StatusData(st));
            }
            wifi::PlayerCommand::SetVolume(vol) => {
                let vol = vol.min(100);
                current_volume = vol;
                set_volume!(spi_cell, spi_config, x_cs, dreq, vol);
            }
            wifi::PlayerCommand::Seek(_) => {}
            wifi::PlayerCommand::Stop => {}

            wifi::PlayerCommand::TogglePlay => {
                normal_play_count = 0;
                if let Some(f) = last_played.clone() {
                    pending = Some(wifi::PlayerCommand::Play(f));
                } else if !files_cache.is_empty() {
                    pending = Some(wifi::PlayerCommand::Play(files_cache[0].clone()));
                }
            }
            wifi::PlayerCommand::NextTrack => {
                normal_play_count = 0;
                if let Some(f) = decide_track(
                    last_played.as_ref(),
                    &files_cache,
                    repeat_mode,
                    play_mode,
                    &mut rng_seed,
                    1,
                ) {
                    pending = Some(wifi::PlayerCommand::Play(f));
                }
            }
            wifi::PlayerCommand::PrevTrack => {
                normal_play_count = 0;
                if let Some(f) = decide_track(
                    last_played.as_ref(),
                    &files_cache,
                    repeat_mode,
                    play_mode,
                    &mut rng_seed,
                    -1,
                ) {
                    pending = Some(wifi::PlayerCommand::Play(f));
                }
            }
            wifi::PlayerCommand::SetRepeatMode => {
                repeat_mode = match repeat_mode {
                    wifi::RepeatMode::Normal => wifi::RepeatMode::RepeatOne,
                    wifi::RepeatMode::RepeatOne => wifi::RepeatMode::RepeatAll,
                    wifi::RepeatMode::RepeatAll => wifi::RepeatMode::Normal,
                };
            }
            wifi::PlayerCommand::SetPlayMode => {
                play_mode = match play_mode {
                    wifi::PlayMode::Sequential => wifi::PlayMode::Random,
                    wifi::PlayMode::Random => wifi::PlayMode::Sequential,
                };
            }
            wifi::PlayerCommand::SaveWifi(s, p) => {
                if let Ok(mut file) =
                    root_dir.open_file_in_dir("WIFI.TXT", Mode::ReadWriteCreateOrTruncate)
                {
                    let mut buf = [0u8; 66];
                    let mut len = 0;
                    for b in s.as_bytes() {
                        buf[len] = *b;
                        len += 1;
                    }
                    buf[len] = b'\n';
                    len += 1;
                    for b in p.as_bytes() {
                        buf[len] = *b;
                        len += 1;
                    }
                    let _ = file.write(&buf[..len]);
                    let _ = file.close();
                }
                cortex_m::peripheral::SCB::sys_reset();
            }

            wifi::PlayerCommand::Play(filename) => {
                info!("Playing {}", filename.as_str());
                last_played = Some(filename.clone());
                let mut cancel = false;

                set_spi_freq!(spi_cell, spi_config, 8_000_000);

                match root_dir.open_file_in_dir(filename.as_str(), Mode::ReadOnly) {
                    Ok(mut file) => {
                        let file_size = file.length();
                        let mut file_pos: u32 = 0;
                        let mut buf = [0u8; 2048];

                        loop {
                            if let Ok(new_cmd) = wifi::CMD_CHANNEL.try_receive() {
                                match new_cmd {
                                    wifi::PlayerCommand::Stop => {
                                        cancel = true;
                                        break;
                                    }
                                    wifi::PlayerCommand::GetStatus => {
                                        rng_seed = next_random(&mut rng_seed);
                                        let st = make_status(
                                            Some(&filename),
                                            current_volume,
                                            file_pos,
                                            file_size,
                                            &files_cache,
                                            repeat_mode,
                                            play_mode,
                                        );
                                        let _ = wifi::RESP_CHANNEL
                                            .try_send(wifi::PlayerResponse::StatusData(st));
                                    }
                                    wifi::PlayerCommand::SetVolume(vol) => {
                                        let vol = vol.min(100);
                                        current_volume = vol;
                                        set_volume!(spi_cell, spi_config, x_cs, dreq, vol);
                                        set_spi_freq!(spi_cell, spi_config, 8_000_000);
                                    }
                                    wifi::PlayerCommand::Seek(pct) => {
                                        let pct = pct.min(100) as u64;
                                        let target = ((file_size as u64 * pct) / 100) as u32;
                                        let target = target.min(file_size);
                                        if file.seek_from_start(target).is_ok() {
                                            file_pos = target;
                                        }
                                    }
                                    wifi::PlayerCommand::Play(new_f) => {
                                        normal_play_count = 0;
                                        pending = Some(wifi::PlayerCommand::Play(new_f));
                                        cancel = true;
                                        break;
                                    }

                                    wifi::PlayerCommand::TogglePlay => {
                                        normal_play_count = 0;
                                        cancel = true; // Stop playing
                                        break;
                                    }
                                    wifi::PlayerCommand::NextTrack => {
                                        normal_play_count = 0;
                                        if let Some(f) = decide_track(
                                            Some(&filename),
                                            &files_cache,
                                            repeat_mode,
                                            play_mode,
                                            &mut rng_seed,
                                            1,
                                        ) {
                                            pending = Some(wifi::PlayerCommand::Play(f));
                                        }
                                        cancel = true;
                                        break;
                                    }
                                    wifi::PlayerCommand::PrevTrack => {
                                        normal_play_count = 0;
                                        if let Some(f) = decide_track(
                                            Some(&filename),
                                            &files_cache,
                                            repeat_mode,
                                            play_mode,
                                            &mut rng_seed,
                                            -1,
                                        ) {
                                            pending = Some(wifi::PlayerCommand::Play(f));
                                        }
                                        cancel = true;
                                        break;
                                    }

                                    wifi::PlayerCommand::SetRepeatMode => {
                                        repeat_mode = match repeat_mode {
                                            wifi::RepeatMode::Normal => wifi::RepeatMode::RepeatOne,
                                            wifi::RepeatMode::RepeatOne => {
                                                wifi::RepeatMode::RepeatAll
                                            }
                                            wifi::RepeatMode::RepeatAll => wifi::RepeatMode::Normal,
                                        };
                                    }
                                    wifi::PlayerCommand::SetPlayMode => {
                                        play_mode = match play_mode {
                                            wifi::PlayMode::Sequential => wifi::PlayMode::Random,
                                            wifi::PlayMode::Random => wifi::PlayMode::Sequential,
                                        };
                                    }
                                    wifi::PlayerCommand::SaveWifi(s, p) => {
                                        pending = Some(wifi::PlayerCommand::SaveWifi(s, p));
                                        cancel = true;
                                        break;
                                    }
                                    wifi::PlayerCommand::SaveAlarm(a) => {
                                        pending = Some(wifi::PlayerCommand::SaveAlarm(a));
                                        cancel = true;
                                        break;
                                    }
                                    wifi::PlayerCommand::CheckAlarm => {}
                                }
                            }

                            match file.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => {
                                    file_pos += n as u32;
                                    let mut offset = 0;
                                    while offset < n {
                                        let chunk = core::cmp::min(32, n - offset);
                                        while dreq.is_low() {
                                            embassy_futures::yield_now().await;
                                        }
                                        x_dcs.set_low();
                                        spi_cell
                                            .borrow_mut()
                                            .blocking_write(&buf[offset..offset + chunk])
                                            .unwrap();
                                        x_dcs.set_high();
                                        offset += chunk;
                                    }
                                }
                                Err(_) => {
                                    error!("SD read error");
                                    break;
                                }
                            }

                            // 讓出 CPU,確保 wifi task 一定有機會執行
                            embassy_futures::yield_now().await;
                        }
                        let _ = file.close();
                    }
                    Err(_) => {
                        error!("Open file failed: {}", filename.as_str());
                    }
                }

                // 收尾:切回低速,必要時 SM_CANCEL,再送 end fill bytes
                set_spi_freq!(spi_cell, spi_config, 1_000_000);

                if !cancel {
                    send_zeros!(spi_cell, x_dcs, dreq, 64);
                    
                    if repeat_mode == wifi::RepeatMode::Normal {
                        normal_play_count += 1;
                    }
                    
                    if repeat_mode != wifi::RepeatMode::Normal || normal_play_count < 20 {
                        // Decide next song
                        if let Some(f) = decide_track(
                            Some(&filename),
                            &files_cache,
                            repeat_mode,
                            play_mode,
                            &mut rng_seed,
                            0,
                        ) {
                            pending = Some(wifi::PlayerCommand::Play(f));
                        }
                    } else {
                        // Finished 20 tracks in Normal mode! Wait for user input.
                        normal_play_count = 0; // optional, but let's reset it or let manual action reset it
                    }
                } else {
                    write_register!(spi_cell, x_cs, dreq, SCI_MODE, MODE_NEW_CANCEL);
                    send_zeros!(spi_cell, x_dcs, dreq, 66);
                }
            }
        }
    }
}
