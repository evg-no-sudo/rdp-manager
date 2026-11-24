use crate::store::{Profile, Result};
use gtk::glib::{KeyFile, KeyFileFlags};
use std::net::Ipv6Addr;

/// Import connection identity only. Credentials and executable options are never read.
pub fn parse(data: &str) -> Result<(Profile, String)> {
    let file = KeyFile::new();
    file.load_from_data(data.trim_start_matches('\u{feff}'), KeyFileFlags::NONE)
        .map_err(|_| "Некорректный формат файла Remmina.")?;
    if !file.has_group("remmina") {
        return Err("Нет секции [remmina].".into());
    }
    let field = |key: &str| -> Result<String> {
        if !file.has_key("remmina", key).unwrap_or(false) {
            return Ok(String::new());
        }
        file.string("remmina", key)
            .map(|s| s.trim().to_string())
            .map_err(|_| "Не удалось прочитать поле профиля.".into())
    };
    if !field("protocol")?.eq_ignore_ascii_case("RDP") {
        return Err("Пропущен профиль: поддерживается только протокол RDP.".into());
    }
    let server = field("server")?;
    let (host, port) = endpoint(&server)?;
    let mut name = field("name")?;
    if name.is_empty() {
        name.clone_from(&host);
    }
    let profile = Profile {
        id: None,
        uid: gtk::glib::uuid_string_random().into(),
        name,
        host,
        port,
        username: field("username")?,
        domain: field("domain")?,
    };
    profile.validate()?;
    Ok((profile, field("group")?))
}

fn endpoint(server: &str) -> Result<(String, u16)> {
    let parse_port = |port: &str| -> Result<u16> {
        port.parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or_else(|| "Некорректный порт в поле server.".into())
    };
    if let Some(rest) = server.strip_prefix('[') {
        let (host, suffix) = rest.split_once(']').ok_or("Некорректный IPv6-адрес.")?;
        host.parse::<Ipv6Addr>()
            .map_err(|_| "Некорректный IPv6-адрес.")?;
        let port = if suffix.is_empty() {
            3389
        } else {
            parse_port(
                suffix
                    .strip_prefix(':')
                    .ok_or("Некорректный адрес сервера.")?,
            )?
        };
        Ok((host.into(), port))
    } else if server.matches(':').count() > 1 {
        server
            .parse::<Ipv6Addr>()
            .map_err(|_| "Некорректный IPv6-адрес. Для порта используйте [IPv6]:порт.")?;
        Ok((server.into(), 3389))
    } else if let Some((host, port)) = server.split_once(':') {
        Ok((host.into(), parse_port(port)?))
    } else {
        Ok((server.into(), 3389))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_basic_fields_without_credentials_or_commands() {
        let (profile, folder) = parse("[remmina]\nprotocol=RDP\nname=Работа\nserver=host.test:3390\nusername=alice\ndomain=EXAMPLE\ngroup=Рабочие\npassword=secret\nprecommand=touch /tmp/unwanted\n").unwrap();
        assert_eq!(folder, "Рабочие");
        assert_eq!(
            profile,
            Profile {
                id: None,
                uid: profile.uid.clone(),
                name: "Работа".into(),
                host: "host.test".into(),
                port: 3390,
                username: "alice".into(),
                domain: "EXAMPLE".into()
            }
        );
    }

    #[test]
    fn supports_ipv4_ipv6_and_name_fallback() {
        for (input, host, port) in [
            ("192.0.2.1", "192.0.2.1", 3389),
            ("[2001:db8::1]:3390", "2001:db8::1", 3390),
            ("[::1]", "::1", 3389),
            ("2001:db8::1", "2001:db8::1", 3389),
        ] {
            let (p, _) = parse(&format!("[remmina]\nprotocol=RDP\nserver={input}")).unwrap();
            assert_eq!(
                (p.host.as_str(), p.port, p.name.as_str()),
                (host, port, host)
            );
        }
    }

    #[test]
    fn rejects_other_protocols_invalid_ports_and_malformed_files() {
        for data in [
            "invalid",
            "[other]\nprotocol=RDP",
            "[remmina]\nprotocol=SSH\nserver=host",
            "[remmina]\nserver=host",
        ] {
            assert!(parse(data).is_err());
        }
        for server in [
            "",
            "host:0",
            "host:65536",
            "host:abc",
            "[::1",
            "[host]:3389",
            "rdp://host",
            "bad host",
            ":3389",
            "[::1]garbage",
        ] {
            assert!(
                parse(&format!("[remmina]\nprotocol=RDP\nserver={server}")).is_err(),
                "{server}"
            );
        }
    }
}
