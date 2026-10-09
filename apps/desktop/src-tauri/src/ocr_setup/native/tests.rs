use super::super::catalog::Catalog;
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
const MAC_MODULES: [&str; 3] = [
    "libopencv_core.4.14.0.dylib",
    "libopencv_imgproc.4.14.0.dylib",
    "libopencv_imgcodecs.4.14.0.dylib",
];
const WORLD: &str = "opencv_world4140.dll";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mado-native-discovery-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn put(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    fn folders(&self) -> NativeSelection {
        NativeSelection::Folders {
            paths: vec![text(&self.0)],
        }
    }

    fn mac_modules(&self, directory: &str, dependencies: &[&str], rpaths: &[&str]) -> Vec<PathBuf> {
        MAC_MODULES
            .iter()
            .map(|name| {
                self.put(
                    &format!("{directory}/{name}"),
                    &mach(name, dependencies, rpaths),
                )
            })
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}
fn platform(os: &str) -> Platform {
    Catalog::load()
        .unwrap()
        .platforms
        .into_iter()
        .find(|platform| platform.os == os)
        .unwrap()
}
fn run(platform: &Platform, selection: &NativeSelection) -> Result<Vec<String>, Fault> {
    discover(platform, selection, &AtomicBool::new(false))
}
fn assert_paths(actual: Vec<String>, expected: Vec<PathBuf>) {
    assert_eq!(
        actual.into_iter().collect::<BTreeSet<_>>(),
        expected.iter().map(|path| text(path)).collect()
    );
}

fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
fn align(size: usize, alignment: usize) -> usize {
    size.div_ceil(alignment) * alignment
}

// Real Mach-O layouts: arm64 MH_DYLIB, __TEXT segment, install name,
// deployment target, aligned dylib/rpath load commands, and a small text body.
fn mach(name: &str, dependencies: &[&str], rpaths: &[&str]) -> Vec<u8> {
    fn command(kind: u32, header: usize, value: &str) -> Vec<u8> {
        let mut bytes = vec![0; align(header + value.len() + 1, 8)];
        let length = bytes.len() as u32;
        put32(&mut bytes, 0, kind);
        put32(&mut bytes, 4, length);
        put32(&mut bytes, 8, header as u32);
        if header == 24 {
            put32(&mut bytes, 16, 0x0004_0e00);
            put32(&mut bytes, 20, 0x0004_0000);
        }
        bytes[header..header + value.len()].copy_from_slice(value.as_bytes());
        bytes
    }
    let mut commands = vec![0; 72];
    put32(&mut commands, 0, 0x19);
    put32(&mut commands, 4, 72);
    commands[8..14].copy_from_slice(b"__TEXT");
    put32(&mut commands, 56, 5);
    put32(&mut commands, 60, 5);
    commands.extend(command(0xd, 24, &format!("@rpath/{name}")));
    let mut build = vec![0; 24];
    put32(&mut build, 0, 0x32);
    put32(&mut build, 4, 24);
    put32(&mut build, 8, 1);
    put32(&mut build, 12, 0x000b_0000);
    put32(&mut build, 16, 0x000e_0000);
    commands.extend(build);
    for dependency in dependencies {
        commands.extend(command(0xc, 24, dependency));
    }
    for rpath in rpaths {
        commands.extend(command(0x8000_001c, 12, rpath));
    }
    let length = align(32 + commands.len() + 16, 4096);
    put64(&mut commands, 32, length as u64);
    put64(&mut commands, 48, length as u64);
    let mut bytes = vec![0; length];
    put32(&mut bytes, 0, 0xfeed_facf);
    put32(&mut bytes, 4, 0x0100_000c);
    put32(&mut bytes, 12, 6);
    put32(
        &mut bytes,
        16,
        (3 + dependencies.len() + rpaths.len()) as u32,
    );
    put32(&mut bytes, 20, commands.len() as u32);
    put32(&mut bytes, 24, 0x85);
    bytes[32..32 + commands.len()].copy_from_slice(&commands);
    bytes[length - 4..].copy_from_slice(&[0xc0, 0x03, 0x5f, 0xd6]);
    bytes
}

fn fat_mach(arm: &[u8]) -> Vec<u8> {
    let x64 = mach("unrelated.dylib", &[], &[]);
    let second = 4096 + x64.len();
    let mut bytes = vec![0; second + arm.len()];
    bytes[..4].copy_from_slice(&[0xca, 0xfe, 0xba, 0xbe]);
    bytes[4..8].copy_from_slice(&2_u32.to_be_bytes());
    for (index, cpu, offset, size) in [
        (0, 0x0100_0007_u32, 4096, x64.len()),
        (1, 0x0100_000c_u32, second, arm.len()),
    ] {
        let start = 8 + index * 20;
        bytes[start..start + 4].copy_from_slice(&cpu.to_be_bytes());
        bytes[start + 8..start + 12].copy_from_slice(&(offset as u32).to_be_bytes());
        bytes[start + 12..start + 16].copy_from_slice(&(size as u32).to_be_bytes());
        bytes[start + 16..start + 20].copy_from_slice(&12_u32.to_be_bytes());
    }
    bytes[4096..second].copy_from_slice(&x64);
    put32(&mut bytes, 4100, 0x0100_0007);
    bytes[second..].copy_from_slice(arm);
    bytes
}

// PE32+ x64 image with .text/.rdata sections and file-backed import name,
// lookup/IAT, delay import and export-forwarder tables. No parser is mocked.
fn pe(imports: &[&str], delays: &[&str], forwards: &[&str]) -> Vec<u8> {
    fn append(data: &mut Vec<u8>, bytes: &[u8]) -> u32 {
        let rva = 0x2000 + data.len() as u32;
        data.extend(bytes);
        rva
    }
    fn cstring(data: &mut Vec<u8>, value: &str) -> u32 {
        let rva = append(data, value.as_bytes());
        data.push(0);
        rva
    }
    let mut data = Vec::new();
    let mut directories = [(0_u32, 0_u32); 16];
    for (names, index, width) in [(imports, 1, 20), (delays, 13, 32)] {
        if names.is_empty() {
            continue;
        }
        let table = data.len();
        let size = (names.len() + 1) * width;
        directories[index] = (append(&mut data, &vec![0; size]), size as u32);
        for (index, name) in names.iter().enumerate() {
            let name = cstring(&mut data, name);
            let hint = append(&mut data, &[0, 0]);
            cstring(&mut data, "fixture_dependency_symbol");
            data.resize(align(data.len(), 8), 0);
            let mut thunk = [0; 16];
            put64(&mut thunk, 0, u64::from(hint));
            let lookup = append(&mut data, &thunk);
            let iat = append(&mut data, &thunk);
            let descriptor = table + index * width;
            if width == 20 {
                put32(&mut data, descriptor, lookup);
                put32(&mut data, descriptor + 12, name);
                put32(&mut data, descriptor + 16, iat);
            } else {
                put32(&mut data, descriptor, 1);
                put32(&mut data, descriptor + 4, name);
                put32(&mut data, descriptor + 12, iat);
                put32(&mut data, descriptor + 16, lookup);
            }
        }
    }
    if !forwards.is_empty() {
        data.resize(align(data.len(), 4), 0);
        let table = data.len();
        let rva = append(&mut data, &[0; 40]);
        let functions_offset = data.len();
        let functions = append(&mut data, &vec![0; forwards.len() * 4]);
        put32(&mut data, table + 16, 1);
        put32(&mut data, table + 20, forwards.len() as u32);
        put32(&mut data, table + 28, functions);
        for (index, forward) in forwards.iter().enumerate() {
            let rva = cstring(&mut data, forward);
            put32(&mut data, functions_offset + index * 4, rva);
        }
        directories[0] = (rva, (data.len() - table) as u32);
    }
    data.resize(align(data.len().max(1), 512), 0);
    let mut bytes = vec![0; 0x600 + data.len()];
    bytes[..2].copy_from_slice(b"MZ");
    put32(&mut bytes, 60, 0x80);
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
    put16(&mut bytes, 0x84, 0x8664);
    put16(&mut bytes, 0x86, 2);
    put16(&mut bytes, 0x94, 240);
    put16(&mut bytes, 0x96, 0x2022);
    let optional = 0x98;
    put16(&mut bytes, optional, 0x20b);
    put32(&mut bytes, optional + 4, 512);
    put32(&mut bytes, optional + 8, data.len() as u32);
    put32(&mut bytes, optional + 16, 0x1000);
    put32(&mut bytes, optional + 20, 0x1000);
    put64(&mut bytes, optional + 24, 0x0001_8000_0000);
    put32(&mut bytes, optional + 32, 4096);
    put32(&mut bytes, optional + 36, 512);
    put16(&mut bytes, optional + 40, 10);
    put16(&mut bytes, optional + 48, 10);
    put32(
        &mut bytes,
        optional + 56,
        (0x2000 + align(data.len(), 4096)) as u32,
    );
    put32(&mut bytes, optional + 60, 0x400);
    put16(&mut bytes, optional + 68, 3);
    put16(&mut bytes, optional + 70, 0x8160);
    put32(&mut bytes, optional + 108, 16);
    for (index, (rva, size)) in directories.into_iter().enumerate() {
        put32(&mut bytes, optional + 112 + index * 8, rva);
        put32(&mut bytes, optional + 116 + index * 8, size);
    }
    for (index, name, rva, size, offset, flags) in [
        (0, ".text", 0x1000, 512, 0x400, 0x6000_0020),
        (1, ".rdata", 0x2000, data.len() as u32, 0x600, 0x4000_0040),
    ] {
        let section = optional + 240 + index * 40;
        bytes[section..section + name.len()].copy_from_slice(name.as_bytes());
        put32(&mut bytes, section + 8, size);
        put32(&mut bytes, section + 12, rva);
        put32(&mut bytes, section + 16, size);
        put32(&mut bytes, section + 20, offset);
        put32(&mut bytes, section + 36, flags);
    }
    bytes[0x400] = 0xc3;
    bytes[0x600..].copy_from_slice(&data);
    bytes
}

#[test]
#[cfg(unix)]
fn homebrew_prefix_resolves_contained_opt_and_cellar_links_without_path_input() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let mut platform = platform("macos");
    platform.native.search_roots = vec![text(&fixture.0)];
    let jpeg = fixture.put(
        "Cellar/jpeg-turbo/3.1/lib/libjpeg.8.dylib",
        &mach("libjpeg.8.dylib", &["/usr/lib/libSystem.B.dylib"], &[]),
    );
    let png = fixture.put(
        "Cellar/libpng/1.6/lib/libpng16.16.dylib",
        &mach(
            "libpng16.16.dylib",
            &["/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation"],
            &[],
        ),
    );
    fs::create_dir(fixture.0.join("opt")).unwrap();
    symlink("../Cellar/jpeg-turbo/3.1", fixture.0.join("opt/jpeg-turbo")).unwrap();
    symlink("../Cellar/libpng/1.6", fixture.0.join("opt/libpng")).unwrap();
    let jpeg_link = text(&fixture.0.join("opt/jpeg-turbo/lib/libjpeg.8.dylib"));
    let png_link = text(&fixture.0.join("opt/libpng/lib/libpng16.16.dylib"));
    let mut expected =
        fixture.mac_modules("Cellar/opencv/4.14.0/lib", &[&jpeg_link, &png_link], &[]);
    symlink("../Cellar/opencv/4.14.0", fixture.0.join("opt/opencv")).unwrap();
    symlink("../Cellar/opencv/4.14.0", fixture.0.join("opt/opencv@4")).unwrap();
    fs::create_dir(fixture.0.join("lib")).unwrap();
    for name in MAC_MODULES {
        symlink(
            format!("../Cellar/opencv/4.14.0/lib/{name}"),
            fixture.0.join("lib").join(name),
        )
        .unwrap();
    }
    fixture.put(
        "lib/libunrelated.dylib",
        b"not a library and never inspected",
    );
    expected.extend([jpeg, png]);
    assert_paths(
        run(&platform, &NativeSelection::Homebrew {}).unwrap(),
        expected,
    );
}

#[test]
fn windows_official_layout_resolves_adjacent_msvc_and_transitive_dependencies() {
    let fixture = Fixture::new();
    let directory = "build/x64/vc16/bin";
    let world = fixture.put(
        &format!("{directory}/{WORLD}"),
        &pe(
            &[
                "KERNEL32.dll",
                "USER32.dll",
                "tbb12.dll",
                "VCRUNTIME140_1.dll",
            ],
            &[],
            &[],
        ),
    );
    let tbb = fixture.put(
        &format!("{directory}/tbb12.dll"),
        &pe(&["VCRUNTIME140.dll"], &[], &[]),
    );
    let runtime1 = fixture.put(
        &format!("{directory}/VCRUNTIME140_1.dll"),
        &pe(&["VCRUNTIME140.dll"], &[], &[]),
    );
    let runtime = fixture.put(
        &format!("{directory}/vcruntime140.dll"),
        &pe(
            &["api-ms-win-crt-runtime-l1-1-0.dll", "ucrtbase.dll"],
            &[],
            &[],
        ),
    );
    fixture.put(
        &format!("{directory}/unrelated.dll"),
        b"ignored unrelated candidate",
    );
    assert_paths(
        run(&platform("windows"), &fixture.folders()).unwrap(),
        vec![world, tbb, runtime1, runtime],
    );
}

#[test]
fn missing_msvc_runtime_requires_an_additional_approved_folder() {
    let fixture = Fixture::new();
    let extra = Fixture::new();
    let world = fixture.put(WORLD, &pe(&["KERNEL32.dll", "MSVCP140.dll"], &[], &[]));
    let runtime = extra.put("bin/msvcp140.dll", &pe(&["ucrtbase.dll"], &[], &[]));
    let platform = platform("windows");
    let error = run(&platform, &fixture.folders()).unwrap_err();
    assert!(error.message.contains("msvcp140.dll"), "{}", error.message);
    assert!(error.message.contains("add its installation folder"));
    assert_eq!(error.context["stage"], "missing");
    let folders = NativeSelection::Folders {
        paths: vec![text(&fixture.0), text(&extra.0)],
    };
    assert_paths(run(&platform, &folders).unwrap(), vec![world, runtime]);
}

#[test]
fn absolute_dependency_outside_approved_roots_is_not_replaced_by_a_same_named_file() {
    let fixture = Fixture::new();
    let extra = Fixture::new();
    let codec = extra.put("libcodec.1.dylib", &mach("libcodec.1.dylib", &[], &[]));
    fixture.put("libcodec.1.dylib", &mach("libcodec.1.dylib", &[], &[]));
    let mut expected = fixture.mac_modules("lib", &[&text(&codec)], &[]);
    let platform = platform("macos");
    let error = run(&platform, &fixture.folders()).unwrap_err();
    assert!(error.message.contains("outside approved folders"));
    assert!(error.message.contains(&text(&codec)));
    expected.push(codec);
    assert_paths(
        run(
            &platform,
            &NativeSelection::Folders {
                paths: vec![text(&fixture.0), text(&extra.0)],
            },
        )
        .unwrap(),
        expected,
    );
}

#[test]
fn unused_executable_runpath_does_not_reject_an_absolute_dependency_closure() {
    let fixture = Fixture::new();
    let dependency = fixture.put(
        "lib/libcodec.1.dylib",
        &mach(
            "libcodec.1.dylib",
            &["/usr/lib/libSystem.B.dylib", "/usr/lib/libc++.1.dylib"],
            &["@executable_path/../lib"],
        ),
    );
    let mut expected = fixture.mac_modules("lib", &[&text(&dependency)], &[]);
    expected.push(dependency);
    assert_paths(
        run(&platform("macos"), &fixture.folders()).unwrap(),
        expected,
    );
}

#[test]
fn executable_runpath_is_refused_when_needed_to_resolve_a_dependency() {
    let fixture = Fixture::new();
    fixture.mac_modules(
        "lib",
        &["@rpath/libcodec.1.dylib"],
        &["@executable_path/../lib", "@loader_path"],
    );
    fixture.put("lib/libcodec.1.dylib", &mach("libcodec.1.dylib", &[], &[]));
    let error = run(&platform("macos"), &fixture.folders()).unwrap_err();
    assert!(error.message.contains("@executable_path/../lib"));
    assert!(error.message.contains("unsupported dependency/runpath"));
}

#[test]
fn declared_loader_and_inherited_rpaths_resolve_a_dependency_cycle() {
    let fixture = Fixture::new();
    let mut expected = fixture.mac_modules(
        "lib",
        &["@rpath/libbridge.1.dylib"],
        &["@loader_path/../private"],
    );
    expected.push(fixture.put(
        "private/libbridge.1.dylib",
        &mach("libbridge.1.dylib", &["@rpath/libcodec.1.dylib"], &[]),
    ));
    expected.push(fixture.put(
        "private/libcodec.1.dylib",
        &mach("libcodec.1.dylib", &["@loader_path/libbridge.1.dylib"], &[]),
    ));
    assert_paths(
        run(&platform("macos"), &fixture.folders()).unwrap(),
        expected,
    );
}

#[test]
fn runpath_order_selects_the_first_match_without_evaluating_unused_fallbacks() {
    let fixture = Fixture::new();
    let mut expected = fixture.mac_modules(
        "lib",
        &["@rpath/libcodec.1.dylib"],
        &[
            "@loader_path/../absent",
            "@loader_path/../a",
            "@executable_path/../lib",
            "@loader_path/../b",
        ],
    );
    expected.push(fixture.put("a/libcodec.1.dylib", &mach("libcodec.1.dylib", &[], &[])));
    fixture.put("b/libcodec.1.dylib", &mach("libcodec.1.dylib", &[], &[]));
    assert_paths(
        run(&platform("macos"), &fixture.folders()).unwrap(),
        expected,
    );
}

#[test]
fn ambiguous_required_modules_are_not_arbitrarily_chosen() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    fixture.put(WORLD, &pe(&[], &[], &[]));
    other.put(WORLD, &pe(&[], &[], &[]));
    let error = run(
        &platform("windows"),
        &NativeSelection::Folders {
            paths: vec![text(&fixture.0), text(&other.0)],
        },
    )
    .unwrap_err();
    assert!(
        error
            .message
            .contains("ambiguous native dependency opencv_world4140.dll")
    );
}

#[test]
fn folders_are_not_recursively_searched_and_required_names_are_reported() {
    let fixture = Fixture::new();
    fixture.put(&format!("unlisted/deep/{WORLD}"), &pe(&[], &[], &[]));
    let error = run(&platform("windows"), &fixture.folders()).unwrap_err();
    assert!(error.message.contains(WORLD));
    assert_eq!(error.context["stage"], "missing");
}

#[test]
#[cfg(unix)]
fn symlink_and_symlink_parent_traversal_cannot_escape_approved_roots() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let escaped = outside.put("libcodec.1.dylib", &mach("libcodec.1.dylib", &[], &[]));
    fs::create_dir(fixture.0.join("lib")).unwrap();
    symlink(&escaped, fixture.0.join("lib/libcodec.1.dylib")).unwrap();
    fixture.mac_modules("lib", &["@loader_path/libcodec.1.dylib"], &[]);
    assert!(
        run(&platform("macos"), &fixture.folders())
            .unwrap_err()
            .message
            .contains("outside approved folders")
    );

    fs::create_dir(outside.0.join("child")).unwrap();
    symlink(outside.0.join("child"), fixture.0.join("lib/redirect")).unwrap();
    fixture.mac_modules("lib", &["@loader_path/redirect/../libcodec.1.dylib"], &[]);
    assert!(
        run(&platform("macos"), &fixture.folders())
            .unwrap_err()
            .message
            .contains("outside approved folders")
    );
}

#[test]
fn missing_absolute_library_has_no_basename_fallback() {
    let fixture = Fixture::new();
    let absent = fixture.0.join("different/libcodec.1.dylib");
    fixture.mac_modules("lib", &[&text(&absent)], &[]);
    fixture.put("lib/libcodec.1.dylib", &mach("libcodec.1.dylib", &[], &[]));
    let error = run(&platform("macos"), &fixture.folders()).unwrap_err();
    assert_eq!(error.context["stage"], "missing");
    assert!(error.message.contains(&text(&absent)));
}

#[test]
fn unknown_mach_loader_context_is_not_guessed_from_candidates() {
    let fixture = Fixture::new();
    fixture.mac_modules("lib", &["@executable_path/libcodec.1.dylib"], &[]);
    fixture.put("lib/libcodec.1.dylib", &mach("libcodec.1.dylib", &[], &[]));
    let error = run(&platform("macos"), &fixture.folders()).unwrap_err();
    assert!(
        error
            .message
            .contains("unsupported dependency/runpath @executable_path/libcodec.1.dylib")
    );
}

#[test]
fn pe_delay_imports_and_export_forwarders_are_part_of_the_closure() {
    let fixture = Fixture::new();
    let world = fixture.put(
        WORLD,
        &pe(
            &[],
            &["vcomp140.dll"],
            &["MSVCP140.some_export", "KERNEL32.Sleep"],
        ),
    );
    let parallel = fixture.put("vcomp140.dll", &pe(&["KERNEL32.dll"], &[], &[]));
    let cpp = fixture.put("msvcp140.dll", &pe(&[], &[], &[]));
    assert_paths(
        run(&platform("windows"), &fixture.folders()).unwrap(),
        vec![world, parallel, cpp],
    );
}

#[test]
fn both_seed_and_transitive_library_architectures_are_checked() {
    let fixture = Fixture::new();
    let mut wrong_pe = pe(&[], &[], &[]);
    put16(&mut wrong_pe, 0x84, 0xaa64);
    fixture.put(WORLD, &wrong_pe);
    assert!(
        run(&platform("windows"), &fixture.folders())
            .unwrap_err()
            .message
            .contains("x64 PE DLL")
    );

    fixture.mac_modules("lib", &["@loader_path/libcodec.1.dylib"], &[]);
    let mut wrong_mach = mach("libcodec.1.dylib", &[], &[]);
    put32(&mut wrong_mach, 4, 0x0100_0007);
    fixture.put("lib/libcodec.1.dylib", &wrong_mach);
    assert!(
        run(&platform("macos"), &fixture.folders())
            .unwrap_err()
            .message
            .contains("arm64 Mach-O dylib")
    );
}

#[test]
fn universal_library_uses_the_arm64_dependency_table_not_another_slice() {
    let fixture = Fixture::new();
    let mut expected = Vec::new();
    for name in MAC_MODULES {
        expected.push(fixture.put(
            &format!("lib/{name}"),
            &fat_mach(&mach(name, &["@loader_path/libcodec.1.dylib"], &[])),
        ));
    }
    expected.push(fixture.put("lib/libcodec.1.dylib", &mach("libcodec.1.dylib", &[], &[])));
    assert_paths(
        run(&platform("macos"), &fixture.folders()).unwrap(),
        expected,
    );
}

#[test]
fn malformed_mach_commands_and_pe_rvas_are_refused() {
    let fixture = Fixture::new();
    let mut malformed = mach(MAC_MODULES[0], &[], &[]);
    put32(&mut malformed, 20, 1_048_577);
    fixture.mac_modules("lib", &[], &[]);
    fixture.put(&format!("lib/{}", MAC_MODULES[0]), &malformed);
    assert!(
        run(&platform("macos"), &fixture.folders())
            .unwrap_err()
            .message
            .contains("load commands exceed bounds")
    );

    let mut malformed = pe(&["KERNEL32.dll"], &[], &[]);
    put32(&mut malformed, 0x600 + 12, 0xffff_ff00);
    fixture.put(WORLD, &malformed);
    assert!(
        run(&platform("windows"), &fixture.folders())
            .unwrap_err()
            .message
            .contains("malformed")
    );
}

#[test]
fn closure_file_and_dependency_name_limits_are_enforced() {
    let fixture = Fixture::new();
    fixture.put(WORLD, &pe(&["dependency0.dll"], &[], &[]));
    for index in 0..64 {
        let imports = if index == 63 {
            Vec::new()
        } else {
            vec![format!("dependency{}.dll", index + 1)]
        };
        let names = imports.iter().map(String::as_str).collect::<Vec<_>>();
        fixture.put(&format!("dependency{index}.dll"), &pe(&names, &[], &[]));
    }
    assert!(
        run(&platform("windows"), &fixture.folders())
            .unwrap_err()
            .message
            .contains("exceeds 64 libraries")
    );

    let dependencies = (0..257)
        .map(|index| format!("/usr/lib/libfixture{index}.dylib"))
        .collect::<Vec<_>>();
    let names = dependencies.iter().map(String::as_str).collect::<Vec<_>>();
    fixture.mac_modules("lib", &names, &[]);
    assert!(
        run(&platform("macos"), &fixture.folders())
            .unwrap_err()
            .message
            .contains("exceeds 256 dependency names")
    );
}

#[test]
fn file_size_and_total_metadata_read_budgets_are_enforced() {
    let fixture = Fixture::new();
    let path = fixture.put(WORLD, &pe(&[], &[], &[]));
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(1_073_741_825)
        .unwrap();
    assert!(
        run(&platform("windows"), &fixture.folders())
            .unwrap_err()
            .message
            .contains("no larger than 1 GiB")
    );
    fixture.put(WORLD, &pe(&[], &[], &vec!["KERNEL32.Sleep"; 10_000]));
    assert!(
        run(&platform("windows"), &fixture.folders())
            .unwrap_err()
            .message
            .contains("32 MiB discovery read budget")
    );
}

#[test]
fn selection_schema_bounds_and_cancellation_are_enforced_before_search() {
    assert!(
        serde_json::from_str::<NativeSelection>(r#"{"method":"homebrew","paths":[]}"#).is_err()
    );
    assert!(
        serde_json::from_str::<NativeSelection>(r#"{"method":"folders","paths":[],"extra":true}"#)
            .is_err()
    );
    let fixture = Fixture::new();
    let platform = platform("windows");
    assert!(
        run(&platform, &NativeSelection::Homebrew {})
            .unwrap_err()
            .message
            .contains("only on macOS")
    );
    assert!(
        run(&platform, &NativeSelection::Folders { paths: Vec::new() })
            .unwrap_err()
            .message
            .contains("between 1 and 8")
    );
    assert!(
        run(
            &platform,
            &NativeSelection::Folders {
                paths: vec![text(&fixture.0); 9]
            }
        )
        .unwrap_err()
        .message
        .contains("between 1 and 8")
    );
    assert!(
        run(
            &platform,
            &NativeSelection::Folders {
                paths: vec!["relative".into()]
            }
        )
        .unwrap_err()
        .message
        .contains("absolute")
    );
    let error = discover(&platform, &fixture.folders(), &AtomicBool::new(true)).unwrap_err();
    assert_eq!(error.category, "OcrSetupCancelled");
}

#[test]
fn all_eight_approved_folders_can_participate_in_discovery() {
    let fixtures = (0..8).map(|_| Fixture::new()).collect::<Vec<_>>();
    let world = fixtures[7].put(WORLD, &pe(&[], &[], &[]));
    let selection = NativeSelection::Folders {
        paths: fixtures.iter().map(|fixture| text(&fixture.0)).collect(),
    };
    assert_paths(run(&platform("windows"), &selection).unwrap(), vec![world]);
}

#[test]
fn pe_import_paths_cannot_escape_approved_folders() {
    let fixture = Fixture::new();
    fixture.put(WORLD, &pe(&["../outside.dll"], &[], &[]));
    let error = run(&platform("windows"), &fixture.folders()).unwrap_err();
    assert!(
        error
            .message
            .contains("unsupported PE dependency name: ../outside.dll")
    );
}

#[test]
#[cfg(unix)]
fn inferred_system_runpaths_are_not_authority() {
    let fixture = Fixture::new();

    fixture.mac_modules("lib", &["@rpath/libnotanOSlibrary.dylib"], &["/usr/lib"]);
    let error = run(&platform("macos"), &fixture.folders()).unwrap_err();
    assert!(error.message.contains("outside approved folders"));
    assert!(error.message.contains("libnotanOSlibrary.dylib"));
}
