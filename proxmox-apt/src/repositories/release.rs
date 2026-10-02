use anyhow::{Error, bail, format_err};

pub use proxmox_apt_api_types::DebianCodename;

/// Get the Debian release the running system is, or is based on.
///
/// On Debian, that's the `VERSION_CODENAME` from `/etc/os-release`. On Debian derivatives
/// (`ID_LIKE` containing `debian`), like Devuan, it's the Debian release their release is based
/// on: by their codename for the derivatives [`DebianCodename::from_derivative`] knows, otherwise
/// by the major version in `/etc/debian_version`.
pub fn get_current_release_codename() -> Result<DebianCodename, Error> {
    let os_release = std::fs::read_to_string("/etc/os-release")
        .map_err(|err| format_err!("unable to read '/etc/os-release' - {}", err))?;
    let debian_version = std::fs::read_to_string("/etc/debian_version").ok();

    parse_release_codename(&os_release, debian_version.as_deref())
}

fn parse_release_codename(
    os_release: &str,
    debian_version: Option<&str>,
) -> Result<DebianCodename, Error> {
    let (mut id, mut id_like, mut codename) = (None, None, None);

    for line in os_release.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim_matches(&['"', '\''][..]);
        let field = match key {
            "ID" => &mut id,
            "ID_LIKE" => &mut id_like,
            "VERSION_CODENAME" => &mut codename,
            _ => continue,
        };
        field.get_or_insert(value);
    }

    let is_derivative = id.is_some_and(|id| id != "debian")
        && id_like.is_some_and(|like| like.split_whitespace().any(|like| like == "debian"));

    if !is_derivative {
        match codename {
            Some(codename) => return Ok(codename.try_into()?),
            None => bail!("unable to parse codename from '/etc/os-release'"),
        }
    }

    let id = id.unwrap_or_default();

    if let Some(base) = codename.and_then(|codename| DebianCodename::from_derivative(id, codename))
    {
        return Ok(base);
    }

    if let Some(base) = debian_version.and_then(codename_from_debian_version) {
        return Ok(base);
    }

    bail!(
        "unable to determine the Debian release '{id}' release '{}' is based on",
        codename.unwrap_or("unknown"),
    );
}

/// Debian release from the contents of `/etc/debian_version`: a version like `13.4`, or a
/// codename like `trixie/sid` on testing and unstable.
fn codename_from_debian_version(debian_version: &str) -> Option<DebianCodename> {
    let debian_version = debian_version.trim();

    if let Some((codename, _)) = debian_version.split_once('/') {
        return DebianCodename::try_from(codename).ok();
    }

    let major: u32 = debian_version.split('.').next()?.parse().ok()?;
    Some(match major {
        5 => DebianCodename::Lenny,
        6 => DebianCodename::Squeeze,
        7 => DebianCodename::Wheezy,
        8 => DebianCodename::Jessie,
        9 => DebianCodename::Stretch,
        10 => DebianCodename::Buster,
        11 => DebianCodename::Bullseye,
        12 => DebianCodename::Bookworm,
        13 => DebianCodename::Trixie,
        14 => DebianCodename::Forky,
        15 => DebianCodename::Duke,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBIAN_TRIXIE: &str = r#"PRETTY_NAME="Debian GNU/Linux 13 (trixie)"
NAME="Debian GNU/Linux"
VERSION_ID="13"
VERSION="13 (trixie)"
VERSION_CODENAME=trixie
DEBIAN_VERSION_FULL=13.4
ID=debian
"#;

    const DEVUAN_EXCALIBUR: &str = r#"PRETTY_NAME="Devuan GNU/Linux 6 (excalibur)"
NAME="Devuan GNU/Linux"
VERSION_ID="6"
VERSION="6 (excalibur)"
VERSION_CODENAME="excalibur"
ID=devuan
ID_LIKE=debian
"#;

    #[test]
    fn debian() {
        let codename = parse_release_codename(DEBIAN_TRIXIE, Some("13.4\n")).unwrap();
        assert_eq!(codename, DebianCodename::Trixie);

        // unknown codenames stay an error on Debian itself
        let unknown = DEBIAN_TRIXIE.replace("=trixie", "=zurich");
        assert!(parse_release_codename(&unknown, Some("13.4\n")).is_err());

        let missing = DEBIAN_TRIXIE.replace("VERSION_CODENAME=trixie\n", "");
        assert!(parse_release_codename(&missing, None).is_err());
    }

    #[test]
    fn known_derivative() {
        let codename = parse_release_codename(DEVUAN_EXCALIBUR, Some("13.7\n")).unwrap();
        assert_eq!(codename, DebianCodename::Trixie);

        // the codename takes precedence over /etc/debian_version
        let codename = parse_release_codename(DEVUAN_EXCALIBUR, Some("12.11\n")).unwrap();
        assert_eq!(codename, DebianCodename::Trixie);
    }

    #[test]
    fn derivative_by_debian_version() {
        let future = DEVUAN_EXCALIBUR.replace("excalibur", "ceres");
        let codename = parse_release_codename(&future, Some("14.1\n")).unwrap();
        assert_eq!(codename, DebianCodename::Forky);

        let codename = parse_release_codename(&future, Some("forky/sid\n")).unwrap();
        assert_eq!(codename, DebianCodename::Forky);

        assert!(parse_release_codename(&future, None).is_err());
        assert!(parse_release_codename(&future, Some("42.0\n")).is_err());
    }

    #[test]
    fn not_debian_based() {
        let other = "ID=fedora\nVERSION_CODENAME=\"\"\n";
        assert!(parse_release_codename(other, None).is_err());
    }
}
