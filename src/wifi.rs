use core::str;
use defmt::*;
use embassy_stm32::usart::BufferedUart;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer, with_timeout};
use heapless::{String, Vec};

#[derive(Clone, Copy, PartialEq, defmt::Format)]
pub enum RepeatMode {
    Normal,
    RepeatOne,
    RepeatAll,
}

#[derive(Clone, Copy, PartialEq, defmt::Format)]
pub enum PlayMode {
    Sequential,
    Random,
}

pub enum PlayerCommand {
    Play(String<12>),
    Stop,
    TogglePlay,
    NextTrack,
    PrevTrack,
    SetVolume(u8), // 0 to 100
    Seek(u8),      // 0 to 100 (%)
    GetStatus,
    SetRepeatMode,
    SetPlayMode,
    SaveWifi(String<32>, String<32>),
    SaveAlarm(Alarm),
    CheckAlarm,
}

#[derive(Clone, Copy)]
pub struct Alarm {
    pub enabled: bool,
    pub hour: u8,
    pub minute: u8,
    pub days: u8, // bitmask: bit 0=Mon, 1=Tue...
}

pub struct Status {
    pub playing: bool,
    pub filename: String<12>,
    pub volume: u8,
    pub progress_pct: u8,
    pub repeat_mode: RepeatMode,
    pub play_mode: PlayMode,
    pub files: Vec<String<12>, 32>,
}

#[allow(dead_code)]
pub enum PlayerResponse {
    StatusData(Status),
    Ack,
}

pub static CMD_CHANNEL: Channel<CriticalSectionRawMutex, PlayerCommand, 2> = Channel::new();
pub static RESP_CHANNEL: Channel<CriticalSectionRawMutex, PlayerResponse, 2> = Channel::new();
pub static TIME_CHANNEL: Channel<CriticalSectionRawMutex, embassy_stm32::rtc::DateTime, 2> = Channel::new();

const HDR_JSON: &str =
    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n";
const HDR_HTML: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n";

// 每次 CIPSEND 的最大長度 (ESP8266 上限 2048,取保守值)
const SEND_CHUNK: usize = 1400;

const INDEX_HTML: &str = r#"<!DOCTYPE html><html><head><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="icon" href="data:,"><title>MP3</title><style>body{font-size:130%;text-align:center;background:#f4f7f6;color:#2c3e50;padding:20px;font-family:-apple-system,sans-serif}h2{color:#3498db;margin-bottom:10px}h3{color:#7f8c8d;font-weight:400}.p{color:#2ecc71;font-weight:bold}button{font-size:130%;padding:10px 5px;margin:0;border-radius:20px;background:#fff;color:#34495e;border:1px solid #bdc3c7;cursor:pointer;box-shadow:0 2px 4px rgba(0,0,0,.1);transition:all .2s}button:active{transform:scale(.95)}.active{background:#e3f2fd;border-color:#bbdefb;color:#1565c0;font-weight:bold;transform:scale(1.02)}.f{padding:15px;background:#fff;margin:10px auto;cursor:pointer;border-radius:10px;border:1px solid #ecf0f1;width:85%;box-sizing:border-box;box-shadow:0 2px 5px rgba(0,0,0,.05)}input[type=range],progress{width:90%;margin:15px auto}progress{cursor:pointer}.al{margin-top:20px;padding-top:20px;border-top:1px solid #ccc}.dcb{display:inline-block;margin:5px}input{font-size:inherit;}</style><script>let $=i=>document.getElementById(i);let q=[],b=0;let rm=['Normal','Repeat_One','Repeat_All'];let pm=['Sequential','Random'];let w=async()=>{if(b||!q.length)return;b=1;let p=q.shift();try{let c=new AbortController();let t=setTimeout(()=>c.abort(),5000);let r=await fetch('/api/'+p,{signal:c.signal});clearTimeout(t);if(p=='status'){let d=await r.json();let s=$('s');s.innerText=d.playing?'Play: '+d.file:'Stop';s.className=d.playing?'p':'';if(document.activeElement!=$('v')){$('v').value=d.vol;$('vt').innerText=d.vol}$('p').value=d.prog;$('tb').innerText=d.playing?'Stop':'Play';$('rb').innerText=rm[d.rm];$('pb').innerText=pm[d.pm];if($('l').children.length!=d.files.length){let l='';d.files.forEach(f=>l+=`<div class="f" id="f_${f}" onclick="a('play?f=${f}')">${f}</div>`);$('l').innerHTML=l}d.files.forEach(f=>{let e=$('f_'+f);if(e)e.className=(f==d.file&&d.playing)?'f active':'f'})}else if(p=='alarm'){let d=await r.json();$('ae').checked=d.e;let hh=d.h<10?'0'+d.h:d.h;let mm=d.m<10?'0'+d.m:d.m;$('at').value=hh+':'+mm;for(let i=0;i<7;i++){$('d'+i).checked=(d.d&(1<<i))!==0;}$('ast').innerText=d.e?`(Set: ${hh}:${mm})`:'(Disabled)';}}catch(e){}b=0;w()};let a=p=>{if(p=='status'&&(b||q.length))return;if(p.startsWith('vol'))q=q.filter(x=>!x.startsWith('vol'));if(p.startsWith('seek'))q=q.filter(x=>!x.startsWith('seek'));q.push(p);if(p!='status'&&p!='alarm'){q=q.filter(x=>x!='status');q.push('status')}w()};let lt=0;let vl=v=>{$('vt').innerText=v;let n=Date.now();if(n-lt>250){lt=n;a('vol?v='+v)}};let sk=(e,el)=>{let r=el.getBoundingClientRect();let p=Math.max(0,Math.min(100,Math.round((e.clientX-r.left)/r.width*100)));el.value=p;a('seek?p='+p)};setInterval(()=>a('status'),2000);setInterval(()=>{let d=new Date();$('clock').innerText=d.getHours().toString().padStart(2,'0')+':'+d.getMinutes().toString().padStart(2,'0')+':'+d.getSeconds().toString().padStart(2,'0');},1000);window.onload=()=>{a('status');a('alarm');};function sa(){$('ast').innerText='Saving...';let e=$('ae').checked?1:0;let t=$('at').value.split(':');let h=t[0]||0;let m=t[1]||0;let d=0;for(let i=0;i<7;i++){if($('d'+i).checked)d|=(1<<i);}a(`savealarm?e=${e}&h=${h}&m=${m}&d=${d}`);setTimeout(()=>a('alarm'), 1000);}</script></head><body><h2>MP3 Player</h2><div id="clock" style="font-size:80%;color:#7f8c8d;margin-bottom:15px;">00:00:00</div><h3 id="s">Stop</h3><progress id="p" value="0" max="100" style="width:90%" onclick="sk(event,this)"></progress><br>Vol: <span id="vt">0</span><br><input type="range" id="v" min="0" max="100" oninput="vl(this.value)" onchange="a('vol?v='+this.value)"><br><div style="display:flex;gap:10px;width:95%;margin:15px auto"><button onclick="a('prev')" style="flex:1">Pre</button><button id="tb" onclick="a('toggle')" style="flex:1">Stop</button><button onclick="a('next')" style="flex:1">Next</button></div><div style="display:flex;gap:10px;width:95%;margin:15px auto"><button id="rb" onclick="a('repeat')" style="flex:1">Normal</button><button id="pb" onclick="a('playmode')" style="flex:1">Sequential</button></div><div class="al"><h3>Alarm <span id="ast" style="font-size:14px;color:#2ecc71;"></span></h3><input type="checkbox" id="ae"> Enable <input type="time" id="at"><br><div><label class="dcb"><input type="checkbox" id="d0">Mo</label><label class="dcb"><input type="checkbox" id="d1">Tu</label><label class="dcb"><input type="checkbox" id="d2">We</label><label class="dcb"><input type="checkbox" id="d3">Th</label><label class="dcb"><input type="checkbox" id="d4">Fr</label><label class="dcb"><input type="checkbox" id="d5">Sa</label><label class="dcb"><input type="checkbox" id="d6">Su</label></div><button onclick="sa()" style="margin-top:15px;width:95%">Save Alarm</button></div><div id="l"></div></body></html>"#;

fn decode_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn url_decode(encoded: &str) -> String<32> {
    let mut out = String::<32>::new();
    let mut bytes = encoded.bytes();
    while let Some(b) = bytes.next() {
        if b == b'+' {
            let _ = out.push(' ');
        } else if b == b'%' {
            if let (Some(h1), Some(h2)) = (bytes.next(), bytes.next()) {
                if let (Some(n1), Some(n2)) = (decode_hex(h1), decode_hex(h2)) {
                    let c = (n1 << 4) | n2;
                    let _ = out.push(c as char);
                }
            }
        } else {
            let _ = out.push(b as char);
        }
    }
    out
}

pub struct Esp8266<'d> {
    uart: BufferedUart<'d>,
    buf: [u8; 2048],
}

/// 從 "GET /path?a=1&b=2 HTTP/1.1" 取出 path
fn request_path(req: &str) -> &str {
    req.lines()
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .unwrap_or("/")
}

/// 從 path 的 query 取出 key 的值
fn query_value<'a>(path: &'a str, key: &str) -> Option<&'a str> {
    let q = path.split_once('?')?.1;
    for kv in q.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            if k == key {
                return Some(v);
            }
        }
    }
    None
}

fn valid_filename(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'_' || c == b'-' || c == b'~')
}

impl<'d> Esp8266<'d> {
    pub fn new(uart: BufferedUart<'d>) -> Self {
        Self {
            uart,
            buf: [0; 2048],
        }
    }

    // ---------- 低階 UART 工具 ----------

    async fn read_byte(&mut self) -> Option<u8> {
        let mut b = [0u8; 1];
        match self.uart.read(&mut b).await {
            Ok(n) if n > 0 => Some(b[0]),
            _ => None,
        }
    }

    async fn read_byte_to(&mut self, ms: u64) -> Option<u8> {
        match with_timeout(Duration::from_millis(ms), self.read_byte()).await {
            Ok(v) => v,
            Err(_) => None,
        }
    }

    /// 確保整包資料都寫出去 (write 可能只寫一部分)
    async fn write_all(&mut self, data: &[u8]) -> Result<(), ()> {
        let mut off = 0;
        while off < data.len() {
            match self.uart.write(&data[off..]).await {
                Ok(n) => off += n,
                Err(_) => return Err(()),
            }
        }
        Ok(())
    }

    /// 清掉 RX 內殘留資料 (只用在初始化階段)
    async fn drain(&mut self) {
        let mut trash = [0u8; 64];
        for _ in 0..64 {
            match with_timeout(Duration::from_millis(10), self.uart.read(&mut trash)).await {
                Ok(Ok(_)) => {}
                _ => break,
            }
        }
    }

    /// 等待 target 出現;遇到 ERROR / FAIL 或逾時回 Err
    async fn wait_resp(&mut self, target: &str, secs: u64) -> Result<usize, ()> {
        let tgt = target.as_bytes();
        if tgt.is_empty() {
            return Err(());
        }

        let res = with_timeout(Duration::from_secs(secs), async {
            let mut pos = 0usize;
            let mut mp = 0usize;
            loop {
                let c = match self.read_byte().await {
                    Some(c) => c,
                    None => {
                        Timer::after_millis(1).await;
                        continue;
                    }
                };
                if pos < self.buf.len() {
                    self.buf[pos] = c;
                    pos += 1;
                }

                if c == tgt[mp] {
                    mp += 1;
                    if mp == tgt.len() {
                        return Ok(pos);
                    }
                } else if c == tgt[0] {
                    mp = 1;
                } else {
                    mp = 0;
                }

                if self.buf[..pos].ends_with(b"ERROR") || self.buf[..pos].ends_with(b"FAIL") {
                    return Err(());
                }
            }
        })
        .await;

        match res {
            Ok(Ok(p)) => Ok(p),
            _ => {
                error!("Timeout or error waiting for {}", target);
                Err(())
            }
        }
    }

    pub async fn send_cmd_ex(
        &mut self,
        cmd: &str,
        wait_for: &str,
        secs: u64,
        drain: bool,
    ) -> Result<usize, ()> {
        if drain {
            self.drain().await;
        }
        info!("AT CMD: {}", cmd);
        self.write_all(cmd.as_bytes()).await?;
        self.write_all(b"\r\n").await?;
        self.wait_resp(wait_for, secs).await
    }

    pub async fn send_cmd(&mut self, cmd: &str, wait_for: &str) -> Result<usize, ()> {
        self.send_cmd_ex(cmd, wait_for, 10, true).await
    }

    // ---------- 初始化 ----------

    /// 讓 ESP8266 以 Station 模式連上路由器,並開啟 port 80 的 TCP server
    pub async fn connect_wifi(&mut self, ssid: &str, pwd: &str) -> Result<(), ()> {
        info!("Waiting for ESP8266 to boot...");
        Timer::after_secs(2).await;

        let mut connected = false;
        for retry in 0..5 {
            if self.send_cmd("AT", "OK").await.is_ok() {
                info!("ESP8266 communication is OK!");
                connected = true;
                break;
            }
            error!(
                "Failed to communicate with ESP8266. Retrying... ({}/5)",
                retry + 1
            );
            Timer::after_millis(500).await;
        }
        if !connected {
            error!("CRITICAL: ESP8266 not responding. Check UART4 wiring!");
            return Err(());
        }

        info!("Resetting ESP8266...");
        let _ = self.send_cmd("AT+RST", "OK").await;
        Timer::after_secs(4).await;

        // 關閉回音,回應較乾淨
        let _ = self.send_cmd("ATE0", "OK").await;

        info!("Setting WiFi mode to Station...");
        self.send_cmd("AT+CWMODE=1", "OK").await?;

        info!("Connecting to WiFi AP (SSID: {})...", ssid);
        let mut connect_cmd = String::<192>::new();
        core::fmt::write(
            &mut connect_cmd,
            format_args!("AT+CWJAP=\"{}\",\"{}\"", ssid, pwd),
        )
        .map_err(|_| ())?;
        // 連線可能要 20 秒以上
        if self
            .send_cmd_ex(&connect_cmd, "OK", 40, true)
            .await
            .is_err()
        {
            error!("Failed to connect to WiFi AP.");
            return Err(());
        }

        info!("Getting IP Address...");
        let n = self.send_cmd("AT+CIFSR", "OK").await?;
        if let Ok(resp) = str::from_utf8(&self.buf[..n]) {
            info!("ESP8266 Network Info:\n{}", resp);
        }

        info!("Starting HTTP Server on port 80...");
        self.send_cmd("AT+CIPMUX=1", "OK").await?;
        self.send_cmd("AT+CIPSERVER=1,80", "OK").await?;
        Ok(())
    }

    // ---------- HTTP server ----------

    /// 分段送出資料 (每段一次 CIPSEND)
    async fn send_data(&mut self, conn_id: u8, data: &[u8]) -> bool {
        for chunk in data.chunks(SEND_CHUNK) {
            let mut c = String::<32>::new();
            if core::fmt::write(
                &mut c,
                format_args!("AT+CIPSEND={},{}", conn_id, chunk.len()),
            )
            .is_err()
            {
                return false;
            }
            if self.send_cmd_ex(&c, ">", 5, false).await.is_err() {
                return false;
            }
            if self.write_all(chunk).await.is_err() {
                return false;
            }
            if self.wait_resp("SEND OK", 5).await.is_err() {
                return false;
            }
        }
        true
    }

    pub async fn get_ntp_time(&mut self, server: &str) -> Option<embassy_stm32::rtc::DateTime> {
        self.drain().await;
        
        let mut cmd = String::<64>::new();
        let _ = core::fmt::write(&mut cmd, format_args!("AT+CIPSTART=4,\"UDP\",\"{}\",123", server));
        if self.send_cmd_ex(&cmd, "OK", 5, true).await.is_err() {
            return None;
        }
        
        if self.send_cmd_ex("AT+CIPSEND=4,48", ">", 2, false).await.is_err() {
            let _ = self.send_cmd_ex("AT+CIPCLOSE=4", "OK", 2, false).await;
            return None;
        }
        
        let mut ntp_req = [0u8; 48];
        ntp_req[0] = 0x1B;
        if self.write_all(&ntp_req).await.is_err() {
            let _ = self.send_cmd_ex("AT+CIPCLOSE=4", "OK", 2, false).await;
            return None;
        }
        
        if self.wait_resp("SEND OK", 3).await.is_err() {
            let _ = self.send_cmd_ex("AT+CIPCLOSE=4", "OK", 2, false).await;
            return None;
        }
        
        let target = b"+IPD,4,48:";
        let mut mp = 0;
        let mut found = false;
        for _ in 0..100 {
            if let Some(c) = self.read_byte_to(50).await {
                if c == target[mp] {
                    mp += 1;
                    if mp == target.len() {
                        found = true;
                        break;
                    }
                } else if c == target[0] {
                    mp = 1;
                } else {
                    mp = 0;
                }
            } else {
                break;
            }
        }
        
        if !found {
            let _ = self.send_cmd_ex("AT+CIPCLOSE=4", "OK", 2, false).await;
            return None;
        }
        
        let mut ntp_resp = [0u8; 48];
        for i in 0..48 {
            if let Some(c) = self.read_byte_to(50).await {
                ntp_resp[i] = c;
            } else {
                let _ = self.send_cmd_ex("AT+CIPCLOSE=4", "OK", 2, false).await;
                return None;
            }
        }
        
        let _ = self.send_cmd_ex("AT+CIPCLOSE=4", "OK", 2, false).await;
        
        let seconds_since_1900 = u32::from_be_bytes([ntp_resp[40], ntp_resp[41], ntp_resp[42], ntp_resp[43]]);
        if seconds_since_1900 == 0 {
            return None;
        }
        
        let unix_seconds = seconds_since_1900.saturating_sub(2208988800);
        let local_seconds = unix_seconds + 8 * 3600;
        
        use chrono::{Datelike, Timelike, Weekday};
        if let Some(dt) = chrono::DateTime::from_timestamp(local_seconds as i64, 0) {
            use embassy_stm32::rtc::{DateTime, DayOfWeek};
            let dow = match dt.weekday() {
                Weekday::Mon => DayOfWeek::Monday,
                Weekday::Tue => DayOfWeek::Tuesday,
                Weekday::Wed => DayOfWeek::Wednesday,
                Weekday::Thu => DayOfWeek::Thursday,
                Weekday::Fri => DayOfWeek::Friday,
                Weekday::Sat => DayOfWeek::Saturday,
                Weekday::Sun => DayOfWeek::Sunday,
            };
            
            return DateTime::from(
                dt.year() as u16,
                dt.month() as u8,
                dt.day() as u8,
                dow,
                dt.hour() as u8,
                dt.minute() as u8,
                dt.second() as u8,
                0
            ).ok();
        }
        
        None
    }

    pub async fn run_server(&mut self, current_ssid: &str, alarm: &mut Alarm) {
        let target = b"+IPD,";
        let mut idle_ticks = 0;

        loop {
            // 1. 等待 "+IPD,"
            let mut mp = 0usize;
            loop {
                let c = match with_timeout(Duration::from_millis(100), self.read_byte()).await {
                    Ok(Some(c)) => c,
                    _ => {
                        if mp == 0 {
                            idle_ticks += 1;
                            if idle_ticks >= 36000 { // Every ~1 hour
                                idle_ticks = 0;
                                if let Some(dt) = self.get_ntp_time("time.stdtime.gov.tw").await {
                                    TIME_CHANNEL.try_send(dt).unwrap_or(());
                                }
                            }
                        }
                        continue;
                    }
                };
                if c == target[mp] {
                    mp += 1;
                    if mp == target.len() {
                        break;
                    }
                } else if c == target[0] {
                    mp = 1;
                } else {
                    mp = 0;
                }
            }

            // 2. 解析 "<id>,<len>:"
            let conn_id = match self.read_byte_to(500).await {
                Some(c) if (b'0'..=b'4').contains(&c) => c - b'0',
                _ => continue,
            };
            if self.read_byte_to(500).await != Some(b',') {
                continue;
            }
            let mut len: usize = 0;
            let mut got_colon = false;
            for _ in 0..6 {
                match self.read_byte_to(500).await {
                    Some(b':') => {
                        got_colon = true;
                        break;
                    }
                    Some(d) if d.is_ascii_digit() => len = len * 10 + (d - b'0') as usize,
                    _ => break,
                }
            }
            if !got_colon {
                continue;
            }

            // 3. 讀取 len bytes 的請求內容 (超出緩衝區的部分丟棄)
            let mut pos = 0usize;
            for _ in 0..len {
                match self.read_byte_to(500).await {
                    Some(c) => {
                        if pos < self.buf.len() {
                            self.buf[pos] = c;
                            pos += 1;
                        }
                    }
                    None => break,
                }
            }

            // 4. 組回應
            let mut html = String::<3072>::new();
            let mut ok_request = false;
            {
                let req = str::from_utf8(&self.buf[..pos]).unwrap_or("");
                if req.starts_with("GET ") {
                    ok_request = true;
                    let path = request_path(req);

                    if path.starts_with("/api/play?") {
                        if let Some(f) = query_value(path, "f") {
                            if valid_filename(f) {
                                let mut name = String::<12>::new();
                                if name.push_str(f).is_ok() {
                                    CMD_CHANNEL.send(PlayerCommand::Play(name)).await;
                                }
                            }
                        }
                        let _ = html.push_str(HDR_JSON);
                        let _ = html.push_str("{\"status\":\"ok\"}");
                    } else if path.starts_with("/api/stop") {
                        CMD_CHANNEL.send(PlayerCommand::Stop).await;
                        let _ = html.push_str(HDR_JSON);
                        let _ = html.push_str("{\"status\":\"ok\"}");
                    } else if path.starts_with("/api/vol") {
                        if let Some(v) = query_value(path, "v") {
                            if let Ok(vol) = v.parse::<u8>() {
                                CMD_CHANNEL.send(PlayerCommand::SetVolume(vol)).await;
                            }
                        }
                        let _ = html.push_str(HDR_JSON);
                        let _ = html.push_str("{\"status\":\"ok\"}");
                    } else if path.starts_with("/api/seek") {
                        if let Some(p) = query_value(path, "p") {
                            if let Ok(pct) = p.parse::<u8>() {
                                CMD_CHANNEL.send(PlayerCommand::Seek(pct.min(100))).await;
                            }
                        }
                        let _ = html.push_str(HDR_JSON);
                        let _ = html.push_str("{\"status\":\"ok\"}");
                    } else if path.starts_with("/api/toggle") {
                        CMD_CHANNEL.send(PlayerCommand::TogglePlay).await;
                        let _ = html.push_str(HDR_JSON);
                        let _ = html.push_str("{\"status\":\"ok\"}");
                    } else if path.starts_with("/api/next") {
                        CMD_CHANNEL.send(PlayerCommand::NextTrack).await;
                        let _ = html.push_str(HDR_JSON);
                        let _ = html.push_str("{\"status\":\"ok\"}");
                    } else if path.starts_with("/api/prev") {
                        CMD_CHANNEL.send(PlayerCommand::PrevTrack).await;
                        let _ = html.push_str(HDR_JSON);
                        let _ = html.push_str("{\"status\":\"ok\"}");
                    } else if path.starts_with("/api/repeat") {
                        CMD_CHANNEL.send(PlayerCommand::SetRepeatMode).await;
                        let _ = html.push_str(HDR_JSON);
                        let _ = html.push_str("{\"status\":\"ok\"}");
                    } else if path.starts_with("/api/playmode") {
                        CMD_CHANNEL.send(PlayerCommand::SetPlayMode).await;
                        let _ = html.push_str(HDR_JSON);
                        let _ = html.push_str("{\"status\":\"ok\"}");
                    } else if path.starts_with("/api/status") {
                        // 清掉過期的回應,再要新的
                        while RESP_CHANNEL.try_receive().is_ok() {}
                        let got = with_timeout(Duration::from_secs(2), async {
                            CMD_CHANNEL.send(PlayerCommand::GetStatus).await;
                            RESP_CHANNEL.receive().await
                        })
                        .await;

                        let _ = html.push_str(HDR_JSON);
                        if let Ok(PlayerResponse::StatusData(st)) = got {
                            let playing = if st.playing { "true" } else { "false" };
                            let _ = core::fmt::write(
                                &mut html,
                                format_args!(
                                    "{{\"playing\":{},\"file\":\"{}\",\"vol\":{},\"prog\":{},\"rm\":{},\"pm\":{},\"files\":[",
                                    playing,
                                    st.filename.as_str(),
                                    st.volume,
                                    st.progress_pct,
                                    st.repeat_mode as u8,
                                    st.play_mode as u8
                                ),
                            );
                            let n = st.files.len();
                            for (i, f) in st.files.iter().enumerate() {
                                let _ = core::fmt::write(
                                    &mut html,
                                    format_args!(
                                        "\"{}\"{}",
                                        f.as_str(),
                                        if i + 1 < n { "," } else { "" }
                                    ),
                                );
                            }
                            let _ = html.push_str("]}");
                        } else {
                            let _ = html.push_str(
                                "{\"playing\":false,\"file\":\"\",\"vol\":0,\"prog\":0,\"rm\":0,\"pm\":0,\"files\":[]}",
                            );
                        }
                    } else if path.starts_with("/api/alarm") {
                        let _ = html.push_str(HDR_JSON);
                        let _ = core::fmt::write(&mut html, format_args!("{{\"e\":{},\"h\":{},\"m\":{},\"d\":{}}}", if alarm.enabled{1}else{0}, alarm.hour, alarm.minute, alarm.days));
                    } else if path.starts_with("/api/savealarm") {
                        alarm.enabled = query_value(path, "e").unwrap_or("0") == "1";
                        alarm.hour = query_value(path, "h").unwrap_or("0").parse().unwrap_or(0);
                        alarm.minute = query_value(path, "m").unwrap_or("0").parse().unwrap_or(0);
                        alarm.days = query_value(path, "d").unwrap_or("0").parse().unwrap_or(0);
                        CMD_CHANNEL.send(PlayerCommand::SaveAlarm(alarm.clone())).await;
                        let _ = html.push_str(HDR_JSON);
                        let _ = html.push_str("{\"status\":\"ok\"}");
                    } else if path.starts_with("/setting") {
                        let _ = html.push_str(HDR_HTML);
                        let _ = core::fmt::write(
                            &mut html,
                            format_args!(
                                r#"<!DOCTYPE html><html><head><meta name="viewport" content="width=device-width,initial-scale=1"><title>WiFi Setting</title><style>body{{text-align:center;background:#f4f7f6;color:#2c3e50;padding:20px;font-family:-apple-system,sans-serif}}h2{{color:#3498db;margin-bottom:20px}}input{{padding:10px;margin:10px 0;border:1px solid #bdc3c7;border-radius:5px;width:80%;max-width:300px;font-size:16px}}button{{font-size:16px;padding:10px 20px;margin:20px 5px;border-radius:20px;background:#3498db;color:#fff;border:none;cursor:pointer;box-shadow:0 2px 4px rgba(0,0,0,.1)}}button.secondary{{background:#ecf0f1;color:#2c3e50;border:1px solid #bdc3c7}}</style></head><body><h2>WiFi Setting</h2><form action="/api/savesetting" method="GET"><input type="text" name="s" placeholder="SSID" value="{}" required><br><input type="password" name="p" placeholder="Password"><br><button type="submit">Save & Reboot</button><button type="button" class="secondary" onclick="location.href='/'">Cancel</button></form></body></html>"#,
                                current_ssid
                            ),
                        );
                    } else if path.starts_with("/api/savesetting") {
                        let ssid = query_value(path, "s").unwrap_or("");
                        let pass = query_value(path, "p").unwrap_or("");
                        let s = url_decode(ssid);
                        let p = url_decode(pass);

                        let mut resp = String::<512>::new();
                        let _ = resp.push_str(HDR_HTML);
                        let _ = resp.push_str("<!DOCTYPE html><html><head><meta http-equiv=\"refresh\" content=\"3;url=/\"></head><body><h2>Saved. Rebooting...</h2></body></html>");
                        let _ = self.send_data(conn_id, resp.as_bytes()).await;

                        let mut close_cmd = String::<32>::new();
                        if core::fmt::write(&mut close_cmd, format_args!("AT+CIPCLOSE={}", conn_id))
                            .is_ok()
                        {
                            let _ = self.send_cmd_ex(&close_cmd, "OK", 3, false).await;
                        }

                        CMD_CHANNEL.send(PlayerCommand::SaveWifi(s, p)).await;
                        continue;
                    } else {
                        let _ = self.send_data(conn_id, HDR_HTML.as_bytes()).await;
                        let _ = self.send_data(conn_id, INDEX_HTML.as_bytes()).await;
                        html.clear();
                    }
                }
            }

            // 5. 送出並關閉連線
            if ok_request && !html.is_empty() {
                let sent = self.send_data(conn_id, html.as_bytes()).await;
                if !sent {
                    warn!("Send failed on connection {}", conn_id);
                }
            }

            let mut close_cmd = String::<32>::new();
            if core::fmt::write(&mut close_cmd, format_args!("AT+CIPCLOSE={}", conn_id)).is_ok() {
                let _ = self.send_cmd_ex(&close_cmd, "OK", 3, false).await;
            }
        }
    }
}
