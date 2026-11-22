//! Pure-Rust Windows PE `.rsrc` COFF resource builder for `assets/ferro.ico`.
//! Works out-of-the-box with `ld.lld` without requiring external `rc.exe` or `windres.exe`.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=assets/ferro.ico");
    println!("cargo:rerun-if-changed=assets/icon_64_rgba.bin");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico_path = manifest_dir.join("assets").join("ferro.ico");
    let Ok(ico) = fs::read(&ico_path) else {
        return;
    };
    if ico.len() < 6 {
        return;
    }

    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    if count == 0 || ico.len() < 6 + count * 16 {
        return;
    }

    // Build RT_GROUP_ICON payload (GRPICONDIR + GRPICONDIRENTRY * count)
    let mut group_icon = Vec::with_capacity(6 + count * 14);
    group_icon.extend_from_slice(&ico[0..6]);
    let mut icon_blobs: Vec<Vec<u8>> = Vec::with_capacity(count);

    for i in 0..count {
        let base = 6 + i * 16;
        let entry = &ico[base..base + 16];
        let bytes_in_res = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as usize;
        let img_offset = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]) as usize;
        if img_offset + bytes_in_res > ico.len() {
            return;
        }
        // First 12 bytes of ICONDIRENTRY match GRPICONDIRENTRY, followed by u16 resource ID (1-based)
        group_icon.extend_from_slice(&entry[0..12]);
        group_icon.extend_from_slice(&((i as u16) + 1).to_le_bytes());
        icon_blobs.push(ico[img_offset..img_offset + bytes_in_res].to_vec());
    }

    // Resources to emit:
    // Type 3 (RT_ICON) -> IDs 1..=count -> Lang 1033 (0x0409)
    // Type 14 (RT_GROUP_ICON) -> ID 1 -> Lang 1033 (0x0409)
    let total_items = count + 1;

    // Layout of .rsrc$01:
    // 1. Root Resource Directory (16 bytes) + 2 entries (Type 3, Type 14) (2 * 8 = 16 bytes) = 32 bytes
    // 2. Type 3 Subdir (16 bytes) + count entries (count * 8 bytes)
    // 3. Type 14 Subdir (16 bytes) + 1 entry (8 bytes) = 24 bytes
    // 4. Language Subdirs: total_items * (16 + 8) = total_items * 24 bytes
    // 5. Data Entries: total_items * 16 bytes
    let off_root = 0u32;
    let off_type3 = 32u32;
    let off_type14 = off_type3 + 16 + (count as u32) * 8;
    let off_langs = off_type14 + 24;
    let off_data_entries = off_langs + (total_items as u32) * 24;
    let rsrc01_size = off_data_entries + (total_items as u32) * 16;

    let mut rsrc01 = Vec::with_capacity(rsrc01_size as usize);
    let push_dir_hdr = |buf: &mut Vec<u8>, id_entries: u16| {
        buf.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
        buf.extend_from_slice(&0u32.to_le_bytes()); // TimeDateStamp
        buf.extend_from_slice(&0u16.to_le_bytes()); // MajorVersion
        buf.extend_from_slice(&0u16.to_le_bytes()); // MinorVersion
        buf.extend_from_slice(&0u16.to_le_bytes()); // NumberOfNamedEntries
        buf.extend_from_slice(&id_entries.to_le_bytes());
    };
    let push_dir_entry = |buf: &mut Vec<u8>, id: u32, offset: u32, is_subdir: bool| {
        buf.extend_from_slice(&id.to_le_bytes());
        let off = if is_subdir { offset | 0x8000_0000 } else { offset };
        buf.extend_from_slice(&off.to_le_bytes());
    };

    // 1. Root directory
    let _ = off_root;
    push_dir_hdr(&mut rsrc01, 2);
    push_dir_entry(&mut rsrc01, 3, off_type3, true);   // RT_ICON
    push_dir_entry(&mut rsrc01, 14, off_type14, true); // RT_GROUP_ICON

    // 2. Type 3 (RT_ICON) directory
    push_dir_hdr(&mut rsrc01, count as u16);
    for i in 0..count {
        let lang_off = off_langs + (i as u32) * 24;
        push_dir_entry(&mut rsrc01, (i as u32) + 1, lang_off, true);
    }

    // 3. Type 14 (RT_GROUP_ICON) directory
    push_dir_hdr(&mut rsrc01, 1);
    let group_lang_off = off_langs + (count as u32) * 24;
    push_dir_entry(&mut rsrc01, 1, group_lang_off, true);

    // 4. Language directories (each points to its 16-byte IMAGE_RESOURCE_DATA_ENTRY)
    for i in 0..total_items {
        push_dir_hdr(&mut rsrc01, 1);
        let data_entry_off = off_data_entries + (i as u32) * 16;
        push_dir_entry(&mut rsrc01, 0x0409, data_entry_off, false);
    }

    // 5. Build .rsrc$02 payload and IMAGE_RESOURCE_DATA_ENTRY records + relocations
    let mut rsrc02 = Vec::new();
    let mut relocs: Vec<u32> = Vec::with_capacity(total_items);

    let mut all_blobs = icon_blobs;
    all_blobs.push(group_icon);

    for (i, blob) in all_blobs.iter().enumerate() {
        while rsrc02.len() % 8 != 0 {
            rsrc02.push(0);
        }
        let payload_off = rsrc02.len() as u32;
        let payload_len = blob.len() as u32;
        rsrc02.extend_from_slice(blob);

        let rva_field_off = off_data_entries + (i as u32) * 16;
        relocs.push(rva_field_off);

        rsrc01.extend_from_slice(&payload_off.to_le_bytes()); // OffsetToData (relocated relative to .rsrc$02 symbol)
        rsrc01.extend_from_slice(&payload_len.to_le_bytes()); // Size
        rsrc01.extend_from_slice(&0u32.to_le_bytes());        // CodePage
        rsrc01.extend_from_slice(&0u32.to_le_bytes());        // Reserved
    }
    while rsrc02.len() % 8 != 0 {
        rsrc02.push(0);
    }

    // Build x86_64 COFF object file (.o)
    // Header: 20 bytes
    // Section Headers: 2 * 40 = 80 bytes (.rsrc$01, .rsrc$02)
    // Data 1: rsrc01
    // Relocs 1: relocs.len() * 10 bytes
    // Data 2: rsrc02
    // Symbol Table: 2 symbols * 18 = 36 bytes (symbol 0 = .rsrc$01, symbol 1 = .rsrc$02)
    // String Table: 4 bytes (size = 4)
    let off_sec1_data = 20 + 80;
    let off_sec1_relocs = off_sec1_data + (rsrc01.len() as u32);
    let off_sec2_data = off_sec1_relocs + (relocs.len() as u32) * 10;
    let off_symtab = off_sec2_data + (rsrc02.len() as u32);

    let mut coff = Vec::new();
    // COFF File Header
    coff.extend_from_slice(&0x8664u16.to_le_bytes()); // Machine: IMAGE_FILE_MACHINE_AMD64
    coff.extend_from_slice(&2u16.to_le_bytes());      // NumberOfSections: 2
    coff.extend_from_slice(&0u32.to_le_bytes());      // TimeDateStamp
    coff.extend_from_slice(&off_symtab.to_le_bytes()); // PointerToSymbolTable
    coff.extend_from_slice(&2u32.to_le_bytes());      // NumberOfSymbols: 2
    coff.extend_from_slice(&0u16.to_le_bytes());      // SizeOfOptionalHeader: 0
    coff.extend_from_slice(&0u16.to_le_bytes());      // Characteristics

    // Section 1 Header: .rsrc$01
    let flags = 0x4000_0040u32; // IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_MEM_READ
    coff.extend_from_slice(b".rsrc$01");
    coff.extend_from_slice(&0u32.to_le_bytes()); // VirtualSize
    coff.extend_from_slice(&0u32.to_le_bytes()); // VirtualAddress
    coff.extend_from_slice(&(rsrc01.len() as u32).to_le_bytes());
    coff.extend_from_slice(&off_sec1_data.to_le_bytes());
    coff.extend_from_slice(&off_sec1_relocs.to_le_bytes());
    coff.extend_from_slice(&0u32.to_le_bytes()); // PointerToLinenumbers
    coff.extend_from_slice(&(relocs.len() as u16).to_le_bytes());
    coff.extend_from_slice(&0u16.to_le_bytes()); // NumberOfLinenumbers
    coff.extend_from_slice(&flags.to_le_bytes());

    // Section 2 Header: .rsrc$02
    coff.extend_from_slice(b".rsrc$02");
    coff.extend_from_slice(&0u32.to_le_bytes());
    coff.extend_from_slice(&0u32.to_le_bytes());
    coff.extend_from_slice(&(rsrc02.len() as u32).to_le_bytes());
    coff.extend_from_slice(&off_sec2_data.to_le_bytes());
    coff.extend_from_slice(&0u32.to_le_bytes());
    coff.extend_from_slice(&0u32.to_le_bytes());
    coff.extend_from_slice(&0u16.to_le_bytes());
    coff.extend_from_slice(&0u16.to_le_bytes());
    coff.extend_from_slice(&flags.to_le_bytes());

    // Section 1 Data
    coff.extend_from_slice(&rsrc01);

    // Section 1 Relocations (IMAGE_REL_AMD64_ADDR32NB = 0x0003 against symbol 1: .rsrc$02)
    for r_off in relocs {
        coff.extend_from_slice(&r_off.to_le_bytes()); // VirtualAddress
        coff.extend_from_slice(&1u32.to_le_bytes());  // SymbolTableIndex = 1 (.rsrc$02)
        coff.extend_from_slice(&3u16.to_le_bytes());  // Type = IMAGE_REL_AMD64_ADDR32NB
    }

    // Section 2 Data
    coff.extend_from_slice(&rsrc02);

    // Symbol 0: .rsrc$01 (Section 1, StorageClass 3 = IMAGE_SYM_CLASS_STATIC)
    coff.extend_from_slice(b".rsrc$01");
    coff.extend_from_slice(&0u32.to_le_bytes()); // Value
    coff.extend_from_slice(&1u16.to_le_bytes()); // SectionNumber = 1
    coff.extend_from_slice(&0u16.to_le_bytes()); // Type
    coff.push(3);                                // StorageClass = STATIC
    coff.push(0);                                // NumberOfAuxSymbols

    // Symbol 1: .rsrc$02 (Section 2, StorageClass 3 = IMAGE_SYM_CLASS_STATIC)
    coff.extend_from_slice(b".rsrc$02");
    coff.extend_from_slice(&0u32.to_le_bytes()); // Value
    coff.extend_from_slice(&2u16.to_le_bytes()); // SectionNumber = 2
    coff.extend_from_slice(&0u16.to_le_bytes()); // Type
    coff.push(3);                                // StorageClass = STATIC
    coff.push(0);                                // NumberOfAuxSymbols

    // String Table (empty: 4-byte length = 4)
    coff.extend_from_slice(&4u32.to_le_bytes());

    // Wrap COFF object in a standard `ar` archive `libferro_icon.a`
    let mut ar = Vec::new();
    ar.extend_from_slice(b"!<arch>\n");
    let header = format!(
        "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
        "ferro_icon.o/",
        "0",
        "0",
        "0",
        "100644",
        coff.len()
    );
    ar.extend_from_slice(header.as_bytes());
    ar.extend_from_slice(&coff);
    if ar.len() % 2 == 1 {
        ar.push(b'\n');
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let lib_path = out_dir.join("libferro_icon.a");
    if fs::write(&lib_path, &ar).is_ok() {
        println!("cargo:rustc-link-search=native={}", out_dir.display());
        println!("cargo:rustc-link-lib=static=ferro_icon");
    }
}
