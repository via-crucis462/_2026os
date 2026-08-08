#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;

use user_lib::{sys_close, sys_openat, sys_read, yield_};

const AT_FDCWD: usize = (-100isize) as usize;
const UPTIME_PATH: &str = "/proc/uptime\0";
const MAX_RETRIES: usize = 10_000;

fn is_separator(byte: u8) -> bool {
    byte == b' ' || byte == b'\t' || byte == b'\n' || byte == b'\r'
}

fn next_field<'a>(bytes: &'a [u8], position: &mut usize) -> Option<&'a [u8]> {
    while *position < bytes.len() && is_separator(bytes[*position]) {
        *position += 1;
    }
    let start = *position;
    while *position < bytes.len() && !is_separator(bytes[*position]) {
        *position += 1;
    }
    (start != *position).then_some(&bytes[start..*position])
}

fn parse_centiseconds(token: &[u8]) -> Option<usize> {
    let dot = token.iter().position(|&byte| byte == b'.')?;
    if dot == 0 || token.len() != dot + 3 {
        return None;
    }

    let mut seconds = 0usize;
    for &byte in &token[..dot] {
        let digit = byte.checked_sub(b'0')?;
        if digit > 9 {
            return None;
        }
        seconds = seconds.checked_mul(10)?.checked_add(digit as usize)?;
    }

    let tens = token[dot + 1].checked_sub(b'0')?;
    let units = token[dot + 2].checked_sub(b'0')?;
    if tens > 9 || units > 9 {
        return None;
    }

    seconds
        .checked_mul(100)?
        .checked_add((tens as usize) * 10 + units as usize)
}

fn read_uptime(buf: &mut [u8]) -> Result<(usize, usize), isize> {
    let fd = sys_openat(AT_FDCWD, UPTIME_PATH, 0, 0);
    if fd < 0 {
        return Err(fd);
    }

    let read_len = sys_read(fd as usize, buf);
    let close_result = sys_close(fd as usize);
    if read_len <= 0 {
        return Err(read_len);
    }
    if close_result < 0 {
        return Err(close_result);
    }

    let output = &buf[..read_len as usize];
    let mut position = 0;
    let uptime =
        parse_centiseconds(next_field(output, &mut position).ok_or(-1isize)?).ok_or(-1isize)?;
    parse_centiseconds(next_field(output, &mut position).ok_or(-1isize)?).ok_or(-1isize)?;
    if next_field(output, &mut position).is_some() {
        return Err(-1);
    }

    Ok((uptime, read_len as usize))
}

#[no_mangle]
fn main() -> i32 {
    let mut first_output = [0u8; 64];
    let (first, first_len) = match read_uptime(&mut first_output) {
        Ok(value) => value,
        Err(error) => {
            println!("uptime_test: first read failed: {}", error);
            return 1;
        }
    };

    let first_text = core::str::from_utf8(&first_output[..first_len]).unwrap_or("<invalid>");
    println!("uptime_test: first={}", first_text);

    let mut second_output = [0u8; 64];
    for _ in 0..MAX_RETRIES {
        yield_();
        let (second, second_len) = match read_uptime(&mut second_output) {
            Ok(value) => value,
            Err(error) => {
                println!("uptime_test: second read failed: {}", error);
                return 1;
            }
        };

        if second < first {
            println!(
                "uptime_test: clock moved backwards ({} -> {})",
                first, second
            );
            return 1;
        }
        if second > first {
            let second_text =
                core::str::from_utf8(&second_output[..second_len]).unwrap_or("<invalid>");
            println!("uptime_test: second={}", second_text);
            println!("uptime_test: PASS");
            return 0;
        }
    }

    println!(
        "uptime_test: value did not advance after {} yields",
        MAX_RETRIES
    );
    1
}
