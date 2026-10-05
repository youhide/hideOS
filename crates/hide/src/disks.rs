//! What a disk holds, as the installer tells a person before it asks
//! anything — "Windows, BitLocker on", "hideOS", "empty" — and where on a
//! disk with Windows hideOS can go. See ARCHITECTURE.md, "Beside Windows".
//!
//! The partition table comes from `sfdisk --json`; the NTFS boot sectors
//! are read by the installer and passed in. Nothing here reads a device.

use serde::Deserialize;

/// The EFI system partition.
pub const ESP_TYPE: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";
/// Windows's data partitions: NTFS, FAT, exFAT, BitLocker.
pub const MICROSOFT_BASIC_DATA: &str = "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7";
/// The small partition Windows makes on its system disk, and nothing else
/// does.
pub const MICROSOFT_RESERVED: &str = "E3C9E316-0B5C-4DB8-817D-F92DF00215AE";
/// Windows's recovery environment.
pub const WINDOWS_RECOVERY: &str = "DE94BBA4-06D1-4D40-A16A-BFD50179D6AC";

/// A partition, as sfdisk lists it; positions in sectors.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Partition {
    pub node: String,
    pub start: u64,
    pub size: u64,
    #[serde(rename = "type")]
    pub type_guid: String,
    #[serde(default)]
    pub name: String,
    /// The partition's unique GUID.
    #[serde(default)]
    pub uuid: String,
}

/// A disk's partition table: GPT only, which is all UEFI boots from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub sector_size: u64,
    pub first_lba: u64,
    pub last_lba: u64,
    pub partitions: Vec<Partition>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DiskError {
    #[error("sfdisk's listing is not what it should be: {0}")]
    Listing(String),
    #[error("the disk has an MBR partition table, not GPT")]
    NotGpt,
}

#[derive(Deserialize)]
struct Listing {
    partitiontable: RawTable,
}

#[derive(Deserialize)]
struct RawTable {
    label: String,
    #[serde(default)]
    firstlba: u64,
    #[serde(default)]
    lastlba: u64,
    #[serde(default = "default_sector")]
    sectorsize: u64,
    #[serde(default)]
    partitions: Vec<Partition>,
}

fn default_sector() -> u64 {
    512
}

/// The table in `sfdisk --json`'s output.
pub fn parse(json: &str) -> Result<Table, DiskError> {
    let listing: Listing =
        serde_json::from_str(json).map_err(|e| DiskError::Listing(e.to_string()))?;
    let raw = listing.partitiontable;
    if raw.label != "gpt" {
        return Err(DiskError::NotGpt);
    }
    let mut partitions = raw.partitions;
    for partition in &mut partitions {
        partition.type_guid = partition.type_guid.to_ascii_uppercase();
    }
    partitions.sort_by_key(|p| p.start);
    Ok(Table {
        sector_size: raw.sectorsize,
        first_lba: raw.firstlba,
        last_lba: raw.lastlba,
        partitions,
    })
}

/// What an NTFS-family boot sector says: a plain volume, or one BitLocker
/// encrypts, whose boot sector names `-FVE-FS-` where NTFS's says `NTFS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Volume {
    Ntfs,
    BitLocker,
}

/// The volume a partition's first sector starts, if it is one of these.
pub fn volume(boot_sector: &[u8]) -> Option<Volume> {
    match boot_sector.get(3..11)? {
        b"NTFS    " => Some(Volume::Ntfs),
        b"-FVE-FS-" => Some(Volume::BitLocker),
        _ => None,
    }
}

/// What a disk holds, in the installer's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holds {
    /// No partitions.
    Empty,
    /// hideOS's own partitions.
    HideOs,
    /// Windows's system: its reserved or recovery partition, beside data;
    /// hideOS too when it was installed beside it.
    Windows { bitlocker: bool, hideos: bool },
    /// NTFS volumes and no Windows system: a data disk of Windows's.
    WindowsData { bitlocker: bool },
    /// Something else: another system, or data.
    Other,
}

/// What `table` holds; `volume_of` reads a partition's boot sector.
pub fn holds(table: &Table, volume_of: impl Fn(&Partition) -> Option<Volume>) -> Holds {
    if table.partitions.is_empty() {
        return Holds::Empty;
    }
    // Windows first: a disk with both is Windows's too, and nothing may
    // treat it as hideOS's alone — erase it as if it were.
    let hideos = table
        .partitions
        .iter()
        .any(|p| p.name == "hideos-root" || p.name == "hideos-esp");
    let volumes: Vec<Volume> = table
        .partitions
        .iter()
        .filter(|p| p.type_guid == MICROSOFT_BASIC_DATA)
        .filter_map(&volume_of)
        .collect();
    let bitlocker = volumes.contains(&Volume::BitLocker);
    let system = table
        .partitions
        .iter()
        .any(|p| p.type_guid == MICROSOFT_RESERVED || p.type_guid == WINDOWS_RECOVERY);
    match (system, hideos, volumes.is_empty()) {
        (true, _, _) => Holds::Windows { bitlocker, hideos },
        (false, true, _) => Holds::HideOs,
        (false, false, false) => Holds::WindowsData { bitlocker },
        (false, false, true) => Holds::Other,
    }
}

impl Holds {
    /// A few words for the installer's list of disks.
    pub fn describe(&self) -> &'static str {
        match self {
            Holds::Empty => "empty",
            Holds::HideOs => "hideOS",
            Holds::Windows {
                bitlocker: true,
                hideos: true,
            } => "Windows, BitLocker on, and hideOS",
            Holds::Windows {
                bitlocker: false,
                hideos: true,
            } => "Windows and hideOS",
            Holds::Windows {
                bitlocker: true,
                hideos: false,
            } => "Windows, BitLocker on",
            Holds::Windows {
                bitlocker: false,
                hideos: false,
            } => "Windows",
            Holds::WindowsData { bitlocker: true } => "Windows data, BitLocker on",
            Holds::WindowsData { bitlocker: false } => "Windows data",
            Holds::Other => "partitions of another system",
        }
    }

    pub fn has_windows(&self) -> bool {
        matches!(self, Holds::Windows { .. })
    }

    pub fn bitlocker(&self) -> bool {
        matches!(
            self,
            Holds::Windows {
                bitlocker: true,
                ..
            } | Holds::WindowsData { bitlocker: true }
        )
    }
}

/// Free space on a disk, in sectors, aligned to 1 MiB as partitioning
/// tools align partitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Free {
    pub start: u64,
    pub sectors: u64,
}

impl Free {
    pub fn bytes(&self, table: &Table) -> u64 {
        self.sectors.saturating_mul(table.sector_size)
    }
}

/// The largest unallocated stretch of `table`, where hideOS can go beside
/// what is there.
pub fn largest_free(table: &Table) -> Option<Free> {
    let align = (1 << 20) / table.sector_size.max(1);
    let up = |lba: u64| lba.div_ceil(align.max(1)) * align.max(1);
    let down = |lba: u64| lba / align.max(1) * align.max(1);
    let mut gaps = Vec::new();
    let mut cursor = table.first_lba;
    for partition in &table.partitions {
        gaps.push((cursor, partition.start));
        cursor = cursor.max(partition.start.saturating_add(partition.size));
    }
    // last_lba is the last usable sector, inclusive.
    gaps.push((cursor, table.last_lba.saturating_add(1)));
    gaps.into_iter()
        .filter_map(|(from, to)| {
            let (start, end) = (up(from), down(to));
            (end > start).then(|| Free {
                start,
                sectors: end - start,
            })
        })
        .max_by_key(|free| free.sectors)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Windows 11 system disk of 256 GiB, as sfdisk lists it, with 64 GiB
    /// left free at the end by Disk Management's "Shrink Volume".
    const WINDOWS: &str = r#"{
       "partitiontable": {
          "label": "gpt", "id": "D4B1-…", "device": "/dev/nvme0n1",
          "unit": "sectors", "firstlba": 34, "lastlba": 536870878,
          "sectorsize": 512,
          "partitions": [
             {"node": "/dev/nvme0n1p1", "start": 2048, "size": 204800,
              "type": "c12a7328-f81f-11d2-ba4b-00a0c93ec93b",
              "uuid": "…", "name": "EFI system partition"},
             {"node": "/dev/nvme0n1p2", "start": 206848, "size": 32768,
              "type": "E3C9E316-0B5C-4DB8-817D-F92DF00215AE",
              "uuid": "…", "name": "Microsoft reserved partition"},
             {"node": "/dev/nvme0n1p3", "start": 239616, "size": 400000000,
              "type": "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7",
              "uuid": "…", "name": "Basic data partition"},
             {"node": "/dev/nvme0n1p4", "start": 400240640, "size": 2097152,
              "type": "DE94BBA4-06D1-4D40-A16A-BFD50179D6AC",
              "uuid": "…"}
          ]
       }
    }"#;

    fn sector(oem: &[u8; 8]) -> Vec<u8> {
        let mut sector = vec![0u8; 512];
        if let Some(field) = sector.get_mut(3..11) {
            field.copy_from_slice(oem);
        }
        sector
    }

    #[test]
    fn a_windows_disk_is_said_so_with_its_bitlocker() {
        let table = parse(WINDOWS).unwrap();
        assert_eq!(table.partitions.len(), 4);
        assert_eq!(
            holds(&table, |_| volume(&sector(b"NTFS    "))),
            Holds::Windows {
                bitlocker: false,
                hideos: false
            }
        );
        let locked = holds(&table, |_| volume(&sector(b"-FVE-FS-")));
        assert_eq!(
            locked,
            Holds::Windows {
                bitlocker: true,
                hideos: false
            }
        );
        assert_eq!(locked.describe(), "Windows, BitLocker on");
    }

    #[test]
    fn the_space_shrink_volume_left_is_found() {
        let table = parse(WINDOWS).unwrap();
        let free = largest_free(&table).unwrap();
        // After the recovery partition, aligned to 1 MiB, to the end.
        assert_eq!(free.start, 402337792);
        assert_eq!(free.start % 2048, 0);
        assert!(free.start >= 400240640 + 2097152);
        assert!(free.start + free.sectors <= 536870878 + 1);
        assert!(free.bytes(&table) > 60 << 30);
    }

    #[test]
    fn empty_hideos_and_data_disks() {
        let empty = r#"{"partitiontable": {"label": "gpt", "firstlba": 34,
            "lastlba": 1000000, "sectorsize": 512}}"#;
        let table = parse(empty).unwrap();
        assert_eq!(holds(&table, |_| None), Holds::Empty);
        assert_eq!(largest_free(&table).map(|f| f.start), Some(2048));

        let hideos = r#"{"partitiontable": {"label": "gpt", "firstlba": 34,
            "lastlba": 1000000, "partitions": [
            {"node": "/dev/vda1", "start": 2048, "size": 1048576,
             "type": "C12A7328-F81F-11D2-BA4B-00A0C93EC93B", "name": "hideos-esp"}]}}"#;
        assert_eq!(holds(&parse(hideos).unwrap(), |_| None), Holds::HideOs);

        let data = r#"{"partitiontable": {"label": "gpt", "firstlba": 34,
            "lastlba": 1000000, "partitions": [
            {"node": "/dev/sdb1", "start": 2048, "size": 900000,
             "type": "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7"}]}}"#;
        assert_eq!(
            holds(&parse(data).unwrap(), |_| volume(&sector(b"NTFS    "))),
            Holds::WindowsData { bitlocker: false }
        );
    }

    #[test]
    fn hideos_beside_windows_is_still_windows() {
        let mut table = parse(WINDOWS).unwrap();
        table.partitions.push(Partition {
            node: "/dev/nvme0n1p5".into(),
            start: 402337792,
            size: 1048576,
            type_guid: ESP_TYPE.into(),
            name: "hideos-esp".into(),
            uuid: String::new(),
        });
        let holds = holds(&table, |_| volume(&sector(b"NTFS    ")));
        assert!(holds.has_windows());
        assert_eq!(holds.describe(), "Windows and hideOS");
    }

    #[test]
    fn mbr_is_not_installed_beside() {
        let mbr = r#"{"partitiontable": {"label": "dos", "partitions": []}}"#;
        assert_eq!(parse(mbr), Err(DiskError::NotGpt));
    }
}
