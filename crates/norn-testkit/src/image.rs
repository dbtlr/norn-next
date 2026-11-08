//! The running executable's own mapped pages: making them all resident, and
//! reading how much of them is.
//!
//! **Why a measured child prefaults its executable.** A child's peak resident
//! set carries the executable's file-backed pages, and how many of them are
//! resident depends on which code the child ran: the kernel maps a page the
//! child touches and, through fault-around, the cached pages in a window
//! around it. Two children of one binary that run different code therefore
//! hold different parts of it, and a difference between their peaks keeps a
//! term that grows with the codebase. [`prefault`] makes **every
//! executable-file mapping** wholly resident, so that term is the whole
//! file's mapped size in every child, by construction.
//!
//! An executable-file mapping is a readable mapping of the executable's own
//! file, whatever its other permissions: code, read-only data such as
//! `.rodata`, `.eh_frame` and `.gcc_except_table`, and the file-backed part of
//! the writable data. Naming them by file rather than by segment is what keeps
//! a linker layout that splits or merges segments covered.
//!
//! On Linux the mappings are read from `/proc/self/maps`, made resident with
//! `madvise(MADV_POPULATE_READ)`, or one byte read per page where the kernel
//! refuses that advice (before 5.14), and their residency is read from
//! `/proc/self/smaps`. On macOS the main image's readable segments are walked
//! from its Mach-O load commands and one byte is read per page of each
//! segment's file-backed part. **Residency is read on Linux only**: macOS
//! reports no per-mapping resident size a process can read about itself
//! without a task port, and its readings gate nothing.

use std::io;

/// How much of the executable's mapped file is resident, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Residency {
    /// The summed size of every executable-file mapping.
    pub mapped: u64,
    /// The summed resident size of those mappings.
    pub resident: u64,
}

/// A child's two residency readings: one taken just after [`prefault`], and
/// one taken as the child ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidencyReport {
    pub after_prefault: Residency,
    pub at_end: Residency,
}

/// The prefix of the line a child prints its [`ResidencyReport`] on.
const REPORT_PREFIX: &str = "executable residency";

impl ResidencyReport {
    /// The line a child prints, which [`ResidencyReport::from_stdout`] reads.
    pub fn line(&self) -> String {
        format!(
            "{REPORT_PREFIX} mapped {} after-prefault {} mapped-at-end {} at-end {}",
            self.after_prefault.mapped,
            self.after_prefault.resident,
            self.at_end.mapped,
            self.at_end.resident
        )
    }

    /// The report a child printed in `stdout`, or `None` when it printed none.
    /// A line that starts like a report and does not parse is an error, and so
    /// is a second report.
    pub fn from_stdout(stdout: &str) -> Result<Option<ResidencyReport>, String> {
        let mut found = None;
        for line in stdout
            .lines()
            .filter(|line| line.starts_with(REPORT_PREFIX))
        {
            let tokens: Vec<&str> = line[REPORT_PREFIX.len()..].split_whitespace().collect();
            let number = |text: &str| {
                text.parse::<u64>()
                    .map_err(|problem| format!("the child reported `{line}`: {problem}"))
            };
            let report = match tokens.as_slice() {
                [
                    "mapped",
                    mapped,
                    "after-prefault",
                    after,
                    "mapped-at-end",
                    mapped_end,
                    "at-end",
                    end,
                ] => ResidencyReport {
                    after_prefault: Residency {
                        mapped: number(mapped)?,
                        resident: number(after)?,
                    },
                    at_end: Residency {
                        mapped: number(mapped_end)?,
                        resident: number(end)?,
                    },
                },
                _ => {
                    return Err(format!(
                        "the child reported `{line}`, which is no residency"
                    ));
                }
            };
            if found.replace(report).is_some() {
                return Err(format!("the child reported its residency twice: {stdout}"));
            }
        }
        Ok(found)
    }

    /// The executable's resident bytes, when every executable-file mapping
    /// was wholly resident both after the prefault and at the end.
    ///
    /// **A partial residency is a failed instrument**, not a cheaper child:
    /// the code term a difference between two children cancels is the whole
    /// file only when both hold all of it. So a mapping that the prefault
    /// left short, or that lost pages before the child ended, fails here and
    /// names the readings, as does a mapping that changed size or reads empty.
    pub fn wholly_resident(&self) -> Result<u64, String> {
        let ResidencyReport {
            after_prefault,
            at_end,
        } = *self;
        let named = || {
            format!(
                "{} of {} bytes resident after the prefault, {} of {} at the end",
                after_prefault.resident, after_prefault.mapped, at_end.resident, at_end.mapped
            )
        };
        if after_prefault.mapped == 0 {
            return Err(format!(
                "the child found no executable-file mapping to measure: {}",
                named()
            ));
        }
        if after_prefault.mapped != at_end.mapped {
            return Err(format!(
                "the executable's mappings changed size while the child ran: {}",
                named()
            ));
        }
        if after_prefault.resident != after_prefault.mapped || at_end.resident != at_end.mapped {
            return Err(format!(
                "the executable was not wholly resident, so the code term does not cancel: {}",
                named()
            ));
        }
        Ok(at_end.resident)
    }
}

/// Make every executable-file mapping of this process wholly resident.
pub fn prefault() -> io::Result<()> {
    platform::prefault()
}

/// How much of this process's executable-file mappings is resident, or
/// `None` on a platform that does not report it.
pub fn residency() -> io::Result<Option<Residency>> {
    platform::residency()
}

/// The executable-file mappings in `maps`, the text of `/proc/self/maps`, as
/// `(start, end)` address pairs: the readable mappings whose path is `exe`.
///
/// A mapping that cannot be read cannot be resident, so a `---p` guard mapping
/// of the file, which some layouts carry, is neither prefaulted nor counted.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn executable_mappings(maps: &str, exe: &str) -> Vec<(usize, usize)> {
    maps.lines()
        .filter_map(|line| {
            let mapping = Mapping::parse(line)?;
            (mapping.path == exe && mapping.readable).then_some((mapping.start, mapping.end))
        })
        .collect()
}

/// The residency `smaps`, the text of `/proc/self/smaps`, gives the readable
/// mappings whose path is `exe`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn executable_residency(smaps: &str, exe: &str) -> Residency {
    let mut residency = Residency {
        mapped: 0,
        resident: 0,
    };
    let mut counting = false;
    for line in smaps.lines() {
        if let Some(mapping) = Mapping::parse(line) {
            counting = mapping.path == exe && mapping.readable;
            if counting {
                residency.mapped += (mapping.end - mapping.start) as u64;
            }
        } else if counting && let Some(kib) = line.strip_prefix("Rss:") {
            let kib = kib.trim().trim_end_matches("kB").trim();
            residency.resident += kib.parse::<u64>().unwrap_or(0) * 1024;
        }
    }
    residency
}

/// One mapping line of `/proc/self/maps` or `/proc/self/smaps`.
struct Mapping<'a> {
    start: usize,
    end: usize,
    readable: bool,
    path: &'a str,
}

impl<'a> Mapping<'a> {
    /// The mapping `line` states, or `None` for a line that states none, such
    /// as an `smaps` field line.
    ///
    /// The first five fields are address range, permissions, offset, device
    /// and inode; the path is everything after them, spaces included.
    fn parse(line: &'a str) -> Option<Mapping<'a>> {
        let mut rest = line;
        let mut fields = [""; 5];
        for field in &mut fields {
            rest = rest.trim_start_matches(' ');
            let end = rest.find(' ').unwrap_or(rest.len());
            *field = &rest[..end];
            rest = &rest[end..];
        }
        let (start, end) = fields[0].split_once('-')?;
        let start = usize::from_str_radix(start, 16).ok()?;
        let end = usize::from_str_radix(end, 16).ok()?;
        let permissions = fields[1].as_bytes();
        if permissions.len() != 4 || end < start {
            return None;
        }
        Some(Mapping {
            start,
            end,
            readable: permissions[0] == b'r',
            path: rest.trim_start_matches(' '),
        })
    }
}

/// Read one byte of every page of `[start, end)`, which makes each page
/// resident.
///
/// # Safety
///
/// Every page of the range must be mapped readable for as long as this runs.
unsafe fn touch(start: usize, end: usize, page: usize) {
    let mut address = start;
    while address < end {
        // SAFETY: the caller guarantees the page is mapped readable.
        unsafe { std::ptr::read_volatile(address as *const u8) };
        address += page;
    }
}

/// The system's page size.
fn page_size() -> usize {
    // SAFETY: `sysconf` reads a system constant and has no preconditions.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(page).unwrap_or(4096)
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{Residency, executable_mappings, executable_residency, page_size, touch};
    use std::io;

    /// This executable's path as `/proc/self/maps` names it.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: this process's own executable.
    fn exe() -> io::Result<String> {
        Ok(std::fs::read_link("/proc/self/exe")?
            .to_string_lossy()
            .into_owned())
    }

    #[allow(clippy::disallowed_methods)] // Harness scaffolding: this process's own memory map.
    pub(super) fn prefault() -> io::Result<()> {
        let maps = std::fs::read_to_string("/proc/self/maps")?;
        let mappings = executable_mappings(&maps, &exe()?);
        if mappings.is_empty() {
            return Err(io::Error::other(
                "no readable mapping of this process's executable in /proc/self/maps",
            ));
        }
        let page = page_size();
        for (start, end) in mappings {
            // SAFETY: the range is a mapping this process holds, read from its
            // own map, and the advice only populates page tables for it.
            let advised = unsafe {
                libc::madvise(
                    start as *mut libc::c_void,
                    end - start,
                    libc::MADV_POPULATE_READ,
                )
            };
            if advised != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EINVAL) {
                    return Err(error);
                }
                // A kernel before 5.14 does not know the advice: read the
                // range one page at a time instead.
                // SAFETY: the range is a readable mapping of the executable,
                // which stays mapped for the life of the process.
                unsafe { touch(start, end, page) };
            }
        }
        Ok(())
    }

    #[allow(clippy::disallowed_methods)] // Harness scaffolding: this process's own memory map.
    pub(super) fn residency() -> io::Result<Option<Residency>> {
        let smaps = std::fs::read_to_string("/proc/self/smaps")?;
        Ok(Some(executable_residency(&smaps, &exe()?)))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Residency, page_size, touch};
    use std::io;

    /// The leading fields of a 64-bit Mach-O header, which are all a walk of
    /// its load commands needs.
    #[repr(C)]
    struct MachHeader64 {
        magic: u32,
        cputype: i32,
        cpusubtype: i32,
        filetype: u32,
        ncmds: u32,
        sizeofcmds: u32,
        flags: u32,
        reserved: u32,
    }

    const MH_MAGIC_64: u32 = 0xfeed_facf;

    // dyld's image queries, declared here because libc marks its own
    // declarations deprecated in favour of a crate this workspace does not
    // carry.
    unsafe extern "C" {
        fn _dyld_get_image_header(image_index: u32) -> *const MachHeader64;
        fn _dyld_get_image_vmaddr_slide(image_index: u32) -> isize;
    }

    pub(super) fn prefault() -> io::Result<()> {
        // SAFETY: image 0 is the main executable, which dyld keeps loaded for
        // the life of the process; its header and load commands are mapped
        // readable, and `sizeofcmds` bounds the walk.
        unsafe {
            let header = _dyld_get_image_header(0);
            if header.is_null() || (*header).magic != MH_MAGIC_64 {
                return Err(io::Error::other("the main image is not a 64-bit Mach-O"));
            }
            let slide = _dyld_get_image_vmaddr_slide(0);
            let page = page_size();
            let mut command = header.add(1).cast::<u8>();
            let end = command.add((*header).sizeofcmds as usize);
            for _ in 0..(*header).ncmds {
                if command >= end {
                    break;
                }
                let load = command.cast::<libc::load_command>().read_unaligned();
                if load.cmd == libc::LC_SEGMENT_64 {
                    let segment = command.cast::<libc::segment_command_64>().read_unaligned();
                    if segment.initprot & libc::VM_PROT_READ != 0 && segment.filesize > 0 {
                        let start = (segment.vmaddr as isize + slide) as usize;
                        // Only the file-backed part: a segment's zero-filled
                        // tail is not the executable's file.
                        touch(start, start + segment.filesize as usize, page);
                    }
                }
                command = command.add(load.cmdsize as usize);
            }
        }
        Ok(())
    }

    pub(super) fn residency() -> io::Result<Option<Residency>> {
        Ok(None)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::Residency;
    use std::io;

    pub(super) fn prefault() -> io::Result<()> {
        Err(io::Error::other(
            "prefaulting the executable is implemented for Linux and macOS only",
        ))
    }

    pub(super) fn residency() -> io::Result<Option<Residency>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/sandbox/bin/memory-0123";

    /// A map with the executable split over four mappings, one of them a
    /// guard, beside a library and an anonymous mapping.
    const MAPS: &str = "\
00400000-00401000 r--p 00000000 08:01 42   /sandbox/bin/memory-0123
00401000-00405000 r-xp 00001000 08:01 42   /sandbox/bin/memory-0123
00405000-00406000 ---p 00005000 08:01 42   /sandbox/bin/memory-0123
00406000-00408000 rw-p 00005000 08:01 42   /sandbox/bin/memory-0123
7f0000000000-7f0000002000 r-xp 00000000 08:01 7   /usr/lib/libc.so.6
7f0000100000-7f0000101000 rw-p 00000000 00:00 0
";

    #[test]
    fn every_readable_mapping_of_the_executable_file_is_prefaulted_and_no_other() {
        assert_eq!(
            executable_mappings(MAPS, EXE),
            vec![
                (0x0040_0000, 0x0040_1000),
                (0x0040_1000, 0x0040_5000),
                (0x0040_6000, 0x0040_8000)
            ]
        );
    }

    #[test]
    fn a_path_with_spaces_is_one_path() {
        let maps = "00400000-00401000 r-xp 00000000 08:01 42   /a b/memory\n";
        assert_eq!(
            executable_mappings(maps, "/a b/memory"),
            vec![(0x0040_0000, 0x0040_1000)]
        );
        assert!(executable_mappings(maps, "/a").is_empty());
    }

    #[test]
    fn residency_sums_the_executables_readable_mappings_only() {
        let smaps = "\
00400000-00401000 r--p 00000000 08:01 42   /sandbox/bin/memory-0123
Size:                  4 kB
Rss:                   4 kB
00401000-00405000 r-xp 00001000 08:01 42   /sandbox/bin/memory-0123
Size:                 16 kB
Rss:                  12 kB
00405000-00406000 ---p 00005000 08:01 42   /sandbox/bin/memory-0123
Rss:                   0 kB
7f0000000000-7f0000002000 r-xp 00000000 08:01 7   /usr/lib/libc.so.6
Rss:                   8 kB
";
        assert_eq!(
            executable_residency(smaps, EXE),
            Residency {
                mapped: 20 * 1024,
                resident: 16 * 1024
            }
        );
    }

    fn report(mapped: u64, after: u64, end: u64) -> ResidencyReport {
        ResidencyReport {
            after_prefault: Residency {
                mapped,
                resident: after,
            },
            at_end: Residency {
                mapped,
                resident: end,
            },
        }
    }

    #[test]
    fn a_wholly_resident_executable_reads_its_resident_bytes() {
        assert_eq!(report(8192, 8192, 8192).wholly_resident(), Ok(8192));
    }

    #[test]
    fn an_executable_short_of_wholly_resident_fails_and_names_the_readings() {
        for short in [report(8192, 4096, 8192), report(8192, 8192, 4096)] {
            let problem = short
                .wholly_resident()
                .expect_err("a partial residency fails");
            assert!(problem.contains("not wholly resident"), "{problem}");
            assert!(problem.contains("4096 of 8192"), "{problem}");
        }
    }

    #[test]
    fn an_empty_or_resized_executable_fails() {
        assert!(report(0, 0, 0).wholly_resident().is_err());
        let mut resized = report(8192, 8192, 8192);
        resized.at_end.mapped = 4096;
        resized.at_end.resident = 4096;
        assert!(resized.wholly_resident().is_err());
    }

    #[test]
    fn a_report_reads_back_as_printed() {
        let printed = report(8192, 8192, 4096);
        let stdout = format!("attached 3 documents\n{}\n", printed.line());
        assert_eq!(ResidencyReport::from_stdout(&stdout), Ok(Some(printed)));
        assert_eq!(
            ResidencyReport::from_stdout("attached 3 documents\n"),
            Ok(None)
        );
        let twice = format!("{}\n{}\n", printed.line(), printed.line());
        assert!(ResidencyReport::from_stdout(&twice).is_err());
        assert!(ResidencyReport::from_stdout("executable residency mapped x").is_err());
    }

    /// The prefault and the reading run against this test binary itself, so
    /// a platform where they disagree fails here rather than in the lane.
    #[cfg(target_os = "linux")]
    #[test]
    fn this_binary_is_wholly_resident_once_prefaulted() {
        prefault().expect("prefaulting this binary");
        let residency = residency()
            .expect("reading residency")
            .expect("Linux reports it");
        assert!(residency.mapped > 0);
        assert_eq!(residency.resident, residency.mapped);
    }
}
