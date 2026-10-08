//! Optional identity from the original owned kernel connector's EDID property.
//! EDID is monitor-provided metadata (possibly kernel-overridden), not device
//! authority. Missing/invalid EDID never removes verified backlight authority.
use roost_shell_control::NativeOutputInfo;
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::BorrowedFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

pub const MAX_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub vendor: String,
    pub product: String,
    pub serial: String,
    /// Intrinsic serial quality only: duplicate devices still need a separate
    /// unique-match check before any future persistent restoration.
    pub meaningful_serial: bool,
    pub sha256: [u8; 32],
    pub blocks: u16,
}

impl From<&Identity> for roost_shell_control::EdidIdentityInfo {
    fn from(identity: &Identity) -> Self {
        Self {
            vendor: identity.vendor.clone(),
            product: identity.product.clone(),
            serial: identity.serial.clone(),
            meaningful_serial: identity.meaningful_serial,
            sha256: identity.sha256,
            blocks: identity.blocks,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub identity: Identity,
    pub owner: NativeOutputInfo,
    pub edid_device: u64,
    pub edid_inode: u64,
}

impl Observation {
    /// Discovery cache only: no filesystem access and no persistent restore
    /// admission. Caller must first gate the current owned KMS FD/active VT.
    pub fn cached_identity(&self, owner: &NativeOutputInfo) -> Option<&Identity> {
        (&self.owner == owner).then_some(&self.identity)
    }
}

fn descriptor_text(raw: &[u8]) -> Result<Option<String>, String> {
    let end = raw.iter().position(|c| *c == b'\n').unwrap_or(raw.len());
    if !raw[..end].iter().all(|c| (0x20..=0x7e).contains(c))
        || !raw[end..]
            .iter()
            .enumerate()
            .all(|(i, c)| (i == 0 && *c == b'\n') || *c == b' ')
    {
        return Err("non-ASCII or malformed EDID identity descriptor".into());
    }
    let text = std::str::from_utf8(&raw[..end])
        .map_err(|_| "non-ASCII EDID descriptor")?
        .trim();
    Ok((!text.is_empty()).then(|| text.to_owned()))
}

fn meaningful_serial(serial: &str) -> bool {
    let text = serial.trim().to_ascii_lowercase();
    let digits = text.strip_prefix("0x").unwrap_or(&text);
    !text.is_empty()
        && !digits.chars().all(|c| c == '0')
        && !matches!(
            text.as_str(),
            "unknown"
                | "none"
                | "n/a"
                | "na"
                | "default"
                | "serial number"
                | "serialnumber"
                | "unspecified"
                | "not specified"
        )
}

pub fn parse(raw: &[u8]) -> Result<Identity, String> {
    if raw.len() < 128 || raw.len() > MAX_BYTES || !raw.len().is_multiple_of(128) {
        return Err("EDID block length outside bound".into());
    }
    if raw[..8] != [0, 255, 255, 255, 255, 255, 255, 0] || raw[18] != 1 || raw[19] > 4 {
        return Err("unsupported EDID header/version".into());
    }
    let mut extensions = usize::from(raw[126]);
    // Linux's primary HF-EEODB rule: first CTA extension, revision >=3,
    // first extended data block tag0x78, payload >=2, nonzero override.
    if extensions > 0 && raw.len() >= 256 {
        let cta = &raw[128..256];
        let collection_end = usize::from(cta[2]);
        let length = usize::from(cta[4] & 0x1f);
        if cta[0] == 2
            && cta[1] >= 3
            && (7..=127).contains(&collection_end)
            && cta[4] >> 5 == 7
            && length >= 2
            && 5 + length <= collection_end
            && cta[5] == 0x78
            && cta[6] != 0
        {
            extensions = usize::from(cta[6]);
        }
    }
    if raw.len() != (extensions + 1) * 128
        || raw.as_chunks::<128>().0.iter().any(|block| {
            block
                .iter()
                .fold(0_u8, |sum, value| sum.wrapping_add(*value))
                != 0
        })
    {
        return Err("EDID extension length/checksum mismatch".into());
    }
    let manufacturer = u16::from_be_bytes([raw[8], raw[9]]);
    if manufacturer & 0x8000 != 0 {
        return Err("invalid EDID manufacturer".into());
    }
    let mut vendor = String::new();
    for shift in [10, 5, 0] {
        let letter = ((manufacturer >> shift) & 31) as u8;
        if !(1..=26).contains(&letter) {
            return Err("invalid EDID manufacturer".into());
        }
        vendor.push(char::from(b'A' + letter - 1));
    }
    let product_code = u16::from_le_bytes([raw[10], raw[11]]);
    let numeric_serial = u32::from_le_bytes(raw[12..16].try_into().unwrap());
    let mut product = None;
    let mut serial = None;
    let mut product_seen = false;
    let mut serial_seen = false;
    for descriptor in raw[54..126].as_chunks::<18>().0 {
        if descriptor[..2] != [0, 0] || !matches!(descriptor[3], 0xfc | 0xff) {
            continue;
        }
        if descriptor[2] != 0 || descriptor[4] != 0 {
            return Err("invalid EDID identity descriptor header".into());
        }
        let (seen, value) = if descriptor[3] == 0xfc {
            (&mut product_seen, &mut product)
        } else {
            (&mut serial_seen, &mut serial)
        };
        if *seen {
            return Err("duplicate EDID identity descriptor".into());
        }
        *seen = true;
        *value = descriptor_text(&descriptor[5..18])?;
    }
    let serial = serial.unwrap_or_else(|| format!("0x{numeric_serial:08x}"));
    Ok(Identity {
        vendor,
        product: product.unwrap_or_else(|| format!("0x{product_code:04x}")),
        meaningful_serial: meaningful_serial(&serial),
        serial,
        sha256: Sha256::digest(raw).into(),
        blocks: (extensions + 1) as u16,
    })
}

fn read(file: &mut impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    file.by_ref()
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("owned EDID exceeds bound".into());
    }
    Ok(bytes)
}

fn stable_read(file: &mut (impl Read + Seek)) -> Result<Vec<u8>, String> {
    let first = read(file)?;
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    if read(file)? != first {
        return Err("owned EDID changed during read".into());
    }
    Ok(first)
}

pub fn read_owned(
    fd: BorrowedFd<'_>,
    owner: &NativeOutputInfo,
) -> Result<Option<Observation>, String> {
    crate::native_output::current(fd, owner)?;
    let path = Path::new(&owner.connector_sysfs).join("edid");
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let before = file.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() {
        return Err("owned EDID is nonregular".into());
    }
    // Sysfs advertises this binary attribute with st_size=0; actual read length
    // and EDID block declarations, never that size, define the data bound.
    let bytes = stable_read(&mut file)?;
    let after = file.metadata().map_err(|e| e.to_string())?;
    let named = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !after.is_file()
        || !named.is_file()
        || (before.dev(), before.ino()) != (after.dev(), after.ino())
        || (before.dev(), before.ino()) != (named.dev(), named.ino())
    {
        return Err("owned EDID object changed during read".into());
    }
    crate::native_output::current(fd, owner)?;
    Ok(parse(&bytes).ok().map(|identity| Observation {
        identity,
        owner: owner.clone(),
        edid_device: before.dev(),
        edid_inode: before.ino(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::os::fd::AsFd;
    use std::os::unix::fs::symlink;

    fn checksum(raw: &mut [u8]) {
        for block in raw.as_chunks_mut::<128>().0 {
            block[127] = 0;
            block[127] = 0_u8.wrapping_sub(block.iter().fold(0_u8, |s, v| s.wrapping_add(*v)));
        }
    }
    fn base() -> Vec<u8> {
        let mut raw = vec![0; 128];
        raw[..8].copy_from_slice(&[0, 255, 255, 255, 255, 255, 255, 0]);
        raw[8..10].copy_from_slice(&((4_u16 << 10) | (5 << 5) | 12).to_be_bytes());
        raw[10..12].copy_from_slice(&0x1234_u16.to_le_bytes());
        raw[12..16].copy_from_slice(&0x1234abcd_u32.to_le_bytes());
        raw[18] = 1;
        raw[19] = 4;
        checksum(&mut raw);
        raw
    }
    fn descriptor(raw: &mut [u8], index: usize, tag: u8, text: &str) {
        assert!(text.len() <= 13);
        let block = &mut raw[54 + index * 18..72 + index * 18];
        block.fill(0);
        block[3] = tag;
        block[5..18].fill(b' ');
        block[5..5 + text.len()].copy_from_slice(text.as_bytes());
        if text.len() < 13 {
            block[5 + text.len()] = b'\n';
        }
        checksum(raw);
    }

    #[test]
    fn actual_fields_and_hash_keep_connector_labels_out_of_identity() {
        let raw = base();
        let identity = parse(&raw).unwrap();
        assert_eq!(
            (&*identity.vendor, &*identity.product, &*identity.serial),
            ("DEL", "0x1234", "0x1234abcd")
        );
        assert!(identity.meaningful_serial);
        assert_eq!(identity.sha256, <[u8; 32]>::from(Sha256::digest(&raw)));
        assert_eq!(identity.blocks, 1);
    }
    #[test]
    fn descriptors_are_trimmed_bounded_ascii_with_truthful_precedence() {
        let mut raw = base();
        descriptor(&mut raw, 0, 0xfc, " Actual panel");
        descriptor(&mut raw, 1, 0xff, "S-123");
        let identity = parse(&raw).unwrap();
        assert_eq!(identity.product, "Actual panel");
        assert_eq!(identity.serial, "S-123");
        assert!(identity.meaningful_serial);
    }
    #[test]
    fn zero_and_generic_serials_remain_truthful_but_are_not_restore_identity() {
        let mut raw = base();
        raw[12..16].fill(0);
        checksum(&mut raw);
        let identity = parse(&raw).unwrap();
        assert_eq!(identity.serial, "0x00000000");
        assert!(!identity.meaningful_serial);
        for text in [
            "0",
            "00000000",
            "Unknown",
            "default",
            "serial number",
            "0x00000000",
        ] {
            let mut raw = base();
            descriptor(&mut raw, 0, 0xff, text);
            assert!(!parse(&raw).unwrap().meaningful_serial, "{text}");
        }
    }
    #[test]
    fn empty_serial_descriptor_uses_actual_nonzero_numeric_fallback() {
        let mut raw = base();
        descriptor(&mut raw, 0, 0xff, " ");
        let identity = parse(&raw).unwrap();
        assert_eq!(identity.serial, "0x1234abcd");
        assert!(identity.meaningful_serial);
    }

    #[test]
    fn malformed_header_version_manufacturer_and_base_checksum_are_refused() {
        for offset in [0, 8, 18, 19, 127] {
            let mut raw = base();
            raw[offset] = if offset == 19 { 5 } else { raw[offset] ^ 0x80 };
            if offset != 127 {
                checksum(&mut raw);
            }
            assert!(parse(&raw).is_err(), "offset{offset}");
        }
    }
    #[test]
    fn truncated_extra_and_overbound_blocks_are_refused() {
        for size in [0, 127, 129, 256, MAX_BYTES + 128] {
            let mut raw = base();
            raw.resize(size, 0);
            assert!(parse(&raw).is_err(), "size{size}");
        }
    }
    #[test]
    fn every_declared_extension_checksum_is_required() {
        let mut raw = base();
        raw.resize(384, 0);
        raw[126] = 2;
        raw[128] = 2;
        raw[256] = 0x70;
        checksum(&mut raw);
        assert_eq!(parse(&raw).unwrap().blocks, 3);
        raw[300] ^= 1;
        assert!(parse(&raw).is_err());
        raw.truncate(256);
        assert!(parse(&raw).is_err());
    }
    #[test]
    fn primary_hdmi_forum_override_count_is_bounded_and_exact() {
        let mut raw = base();
        raw.resize(384, 0);
        raw[126] = 1;
        raw[128..135].copy_from_slice(&[2, 3, 7, 0, 0xe2, 0x78, 2]);
        checksum(&mut raw);
        assert_eq!(parse(&raw).unwrap().blocks, 3);
        raw.truncate(256);
        assert!(parse(&raw).is_err());
        raw[130] = 0;
        checksum(&mut raw);
        // Linux rejects a data collection when its offset is below4.
        assert_eq!(parse(&raw).unwrap().blocks, 2);
    }
    #[test]
    fn duplicate_product_or_serial_descriptors_are_refused_even_if_equal() {
        for tag in [0xfc, 0xff] {
            let mut raw = base();
            descriptor(&mut raw, 0, tag, "identity");
            descriptor(&mut raw, 1, tag, "identity");
            assert!(parse(&raw).is_err());
        }
    }
    #[test]
    fn controls_bad_padding_and_reserved_descriptor_bytes_are_refused() {
        for offset in [56, 58, 60, 65] {
            let mut raw = base();
            descriptor(&mut raw, 0, 0xff, "S");
            raw[offset] = 1;
            checksum(&mut raw);
            assert!(parse(&raw).is_err(), "offset{offset}");
        }
    }
    #[test]
    fn maximum_declared_record_is_bounded_without_guessing_missing_extensions() {
        let mut raw = base();
        raw.resize(MAX_BYTES, 0);
        raw[126] = 255;
        checksum(&mut raw);
        assert_eq!(parse(&raw).unwrap().blocks, 256);
    }
    #[test]
    fn same_meaningful_serial_on_two_devices_is_not_a_global_uniqueness_claim() {
        let first = parse(&base()).unwrap();
        let mut other = base();
        other[20] ^= 1;
        checksum(&mut other);
        let second = parse(&other).unwrap();
        assert!(first.meaningful_serial && second.meaningful_serial);
        assert_eq!(first.serial, second.serial);
        assert_ne!(first.sha256, second.sha256);
    }

    // Controlled files exercise admission policy, never physical GPU proof.
    fn owner(root: &Path) -> NativeOutputInfo {
        let path = root.join("original-connector");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("connector_id"), "39").unwrap();
        std::fs::write(path.join("status"), "connected").unwrap();
        std::fs::write(path.join("edid"), base()).unwrap();
        let metadata = path.metadata().unwrap();
        NativeOutputInfo {
            name: "eDP-1".into(),
            drm_device: libc::makedev(1, 3),
            connector_id: 39,
            connector_sysfs: path.to_str().unwrap().into(),
            connector_device: metadata.dev(),
            connector_inode: metadata.ino(),
        }
    }
    #[test]
    fn optional_missing_or_invalid_edid_does_not_modify_original_owner_authority() {
        let tmp = tempfile::tempdir().unwrap();
        let owner = owner(tmp.path());
        let original = owner.clone();
        let fd = std::fs::File::open("/dev/null").unwrap();
        assert!(read_owned(fd.as_fd(), &owner).unwrap().is_some());
        std::fs::write(Path::new(&owner.connector_sysfs).join("edid"), [0; 128]).unwrap();
        assert!(read_owned(fd.as_fd(), &owner).unwrap().is_none());
        std::fs::remove_file(Path::new(&owner.connector_sysfs).join("edid")).unwrap();
        assert!(read_owned(fd.as_fd(), &owner).unwrap().is_none());
        assert_eq!(owner, original);
        assert!(crate::native_output::current(fd.as_fd(), &owner).is_ok());
    }
    #[test]
    fn wrong_gpu_disconnected_or_replaced_connector_cannot_supply_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let owner = owner(tmp.path());
        let fd = std::fs::File::open("/dev/null").unwrap();
        let mut other_gpu = owner.clone();
        other_gpu.drm_device = libc::makedev(226, 1);
        assert!(read_owned(fd.as_fd(), &other_gpu).is_err());
        std::fs::write(
            Path::new(&owner.connector_sysfs).join("status"),
            "disconnected",
        )
        .unwrap();
        assert!(read_owned(fd.as_fd(), &owner).is_err());
        std::fs::write(
            Path::new(&owner.connector_sysfs).join("status"),
            "connected",
        )
        .unwrap();
        std::fs::write(Path::new(&owner.connector_sysfs).join("connector_id"), "51").unwrap();
        assert!(read_owned(fd.as_fd(), &owner).is_err());
        std::fs::rename(&owner.connector_sysfs, tmp.path().join("retired")).unwrap();
        let replacement = super::tests::owner(tmp.path());
        assert_ne!(replacement.connector_inode, owner.connector_inode);
        assert!(read_owned(fd.as_fd(), &owner).is_err());
    }
    #[test]
    fn symlink_nonregular_and_oversized_edid_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let owner = owner(tmp.path());
        let fd = std::fs::File::open("/dev/null").unwrap();
        let path = Path::new(&owner.connector_sysfs).join("edid");
        std::fs::write(&path, vec![0; MAX_BYTES + 1]).unwrap();
        assert!(read_owned(fd.as_fd(), &owner).is_err());
        std::fs::remove_file(&path).unwrap();
        symlink("status", &path).unwrap();
        assert!(read_owned(fd.as_fd(), &owner).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(read_owned(fd.as_fd(), &owner).is_err());
    }
    #[test]
    fn cached_metadata_does_not_reread_connector_or_edid_files() {
        let tmp = tempfile::tempdir().unwrap();
        let owner = owner(tmp.path());
        let observation = Observation {
            identity: parse(&base()).unwrap(),
            owner: owner.clone(),
            edid_device: 1,
            edid_inode: 2,
        };
        std::fs::remove_dir_all(&owner.connector_sysfs).unwrap();
        // Publication is explicitly cached, not fresh restore admission. The
        // caller's active VT/original KMS FD gate remains separately required.
        assert_eq!(
            observation.cached_identity(&owner),
            Some(&observation.identity)
        );
    }
    #[test]
    fn cached_metadata_never_moves_to_a_different_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let original = owner(tmp.path());
        let observation = Observation {
            identity: parse(&base()).unwrap(),
            owner: original.clone(),
            edid_device: 1,
            edid_inode: 2,
        };
        let mut changed = Vec::new();
        let mut other = original.clone();
        other.name = "DP-1".into();
        changed.push(other);
        let mut other = original.clone();
        other.drm_device += 1;
        changed.push(other);
        let mut other = original.clone();
        other.connector_id += 1;
        changed.push(other);
        let mut other = original.clone();
        other.connector_device += 1;
        changed.push(other);
        let mut other = original.clone();
        other.connector_inode += 1;
        changed.push(other);
        let mut other = original.clone();
        other.connector_sysfs.push_str("-replacement");
        changed.push(other);
        for other in changed {
            assert!(observation.cached_identity(&other).is_none());
        }
    }

    struct ChangedOnRewind {
        bytes: Cursor<Vec<u8>>,
        replacement: Vec<u8>,
    }
    impl Read for ChangedOnRewind {
        fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
            self.bytes.read(into)
        }
    }
    impl Seek for ChangedOnRewind {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.bytes = Cursor::new(self.replacement.clone());
            self.bytes.seek(position)
        }
    }
    #[test]
    fn changed_contents_on_same_original_fd_refuse_observation() {
        let original = base();
        let mut changed = original.clone();
        changed[20] ^= 1;
        checksum(&mut changed);
        let mut input = ChangedOnRewind {
            bytes: Cursor::new(original),
            replacement: changed,
        };
        assert!(stable_read(&mut input).is_err());
    }
}
