//! 终端宿主：按键编码等共享工具（终端管线见 term_frame / glue_ssh）。

/// 把 Slint 按键事件编码为终端字节流。
pub fn encode_key(text: &str, ctrl: bool, alt: bool) -> Vec<u8> {
    let csi_arrow = |ch: u8, ctrl: bool, alt: bool| -> Vec<u8> {
        if ctrl || alt {
            let m = 1 + (alt as u8) * 2 + (ctrl as u8) * 4;
            format!("\u{1b}[1;{}{}", m, ch as char).into_bytes()
        } else {
            vec![0x1b, b'[', ch]
        }
    };
    let csi_tilde = |n: u8| -> Vec<u8> { format!("\u{1b}[{}~", n).into_bytes() };
    let ss3 = |ch: u8| -> Vec<u8> { vec![0x1b, b'O', ch] };

    match text {
        "\u{f700}" => return csi_arrow(b'A', ctrl, alt), // Up
        "\u{f701}" => return csi_arrow(b'B', ctrl, alt), // Down
        "\u{f702}" => return csi_arrow(b'D', ctrl, alt), // Left
        "\u{f703}" => return csi_arrow(b'C', ctrl, alt), // Right
        "\u{f727}" => return csi_tilde(2),               // Insert
        "\u{f728}" => return csi_tilde(3),               // Delete
        "\u{f729}" => return csi_arrow(b'H', ctrl, alt), // Home
        "\u{f72b}" => return csi_arrow(b'F', ctrl, alt), // End
        "\u{f72c}" => return csi_tilde(5),               // PageUp
        "\u{f72d}" => return csi_tilde(6),               // PageDown
        "\u{f704}" => return ss3(b'P'),                  // F1
        "\u{f705}" => return ss3(b'Q'),                  // F2
        "\u{f706}" => return ss3(b'R'),                  // F3
        "\u{f707}" => return ss3(b'S'),                  // F4
        "\u{f708}" => return csi_tilde(15),              // F5
        "\u{f709}" => return csi_tilde(17),              // F6
        "\u{f70a}" => return csi_tilde(18),              // F7
        "\u{f70b}" => return csi_tilde(19),              // F8
        "\u{f70c}" => return csi_tilde(20),              // F9
        "\u{f70d}" => return csi_tilde(21),              // F10
        "\u{f70e}" => return csi_tilde(23),              // F11
        "\u{f70f}" => return csi_tilde(24),              // F12
        "\r" | "\u{f719}" => return vec![0x0d],
        "\u{8}" | "\u{7f}" => return vec![0x7f],
        "\u{1b}" => return vec![0x1b],
        "\t" => return vec![0x09],
        _ => {}
    }

    // Ctrl 组合键
    if ctrl {
        if let Some(c) = text.chars().next() {
            if c.is_ascii() {
                let b = c as u8;
                let mapped: Option<u8> = match b {
                    b'@' | b' ' => Some(0x00),
                    b'a'..=b'z' => Some(b - b'a' + 1),
                    b'A'..=b'Z' => Some(b - b'A' + 1),
                    b'[' => Some(0x1b),
                    b'\\' => Some(0x1c),
                    b']' => Some(0x1d),
                    b'^' => Some(0x1e),
                    b'_' | b'/' => Some(0x1f),
                    _ => None,
                };
                if let Some(v) = mapped {
                    return vec![v];
                }
            }
        }
    }

    let mut out = Vec::new();
    if alt {
        out.push(0x1b);
    }
    out.extend_from_slice(text.as_bytes());
    out
}
