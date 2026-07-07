// rockhopper/src/elf_loader.rs
//! Complete ELF loader implementation
//!
//! Handles:
//! - ELF binary parsing and validation
//! - Segment and section extraction
//! - Symbol resolution and relocation
//! - Dynamic linking support
//! - PIE (Position Independent Executable) handling
//! - Load bias calculation for ASLR compatibility

use anyhow::{anyhow, Context, Result};
use goblin::elf::header::{ET_DYN, ET_EXEC};
use goblin::elf::program_header::{PT_DYNAMIC, PT_GNU_RELRO, PT_INTERP, PT_LOAD};
use goblin::elf::section_header::SHT_RELA;
use goblin::elf::Elf;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use tracing::{debug, info, warn};

/// ELF binary loader
pub struct ElfLoader;

/// Loaded ELF binary with all metadata
#[derive(Debug, Clone)]
pub struct LoadedBinary {
    pub path: String,
    pub entry_point: u64,
    pub is_pie: bool,
    pub load_bias: u64,
    pub segments: Vec<Segment>,
    pub sections: Vec<Section>,
    pub symbols: Vec<Symbol>,
    pub relocations: Vec<Relocation>,
    pub dynamic_info: Option<DynamicInfo>,
}

/// Memory segment (from program header)
#[derive(Debug, Clone)]
pub struct Segment {
    pub segment_type: u32,
    pub virtual_address: u64,
    pub physical_address: u64,
    pub file_offset: u64,
    pub file_size: u64,
    pub memory_size: u64,
    pub flags: u32,
    pub alignment: u64,
}

/// Section metadata
#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub section_type: u32,
    pub virtual_address: u64,
    pub file_offset: u64,
    pub size: u64,
    pub alignment: u64,
    pub flags: u64,
}

/// Symbol table entry
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub value: u64,
    pub size: u32,
    pub binding: SymbolBinding,
    pub symbol_type: SymbolType,
    pub visibility: SymbolVisibility,
    pub section_index: u16,
}

/// Symbol binding (global/weak/local)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolBinding {
    Local,
    Global,
    Weak,
    Other(u8),
}

/// Symbol type (function/variable/etc)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolType {
    NoType,
    Object,
    Function,
    Section,
    File,
    Other(u8),
}

/// Symbol visibility
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolVisibility {
    Default,
    Internal,
    Hidden,
    Protected,
}

/// Relocation entry
#[derive(Debug, Clone)]
pub struct Relocation {
    pub offset: u64,
    pub symbol_index: u32,
    pub relocation_type: u32,
    pub addend: Option<i64>,
}

/// Dynamic linking information
#[derive(Debug, Clone)]
pub struct DynamicInfo {
    pub interpreter: Option<String>,
    pub needed_libraries: Vec<String>,
    pub rpath: Vec<String>,
    pub runpath: Vec<String>,
    pub soname: Option<String>,
    pub gnu_hash_offset: Option<u64>,
    pub hash_offset: Option<u64>,
    pub symtab_offset: Option<u64>,
    pub strtab_offset: Option<u64>,
}

impl ElfLoader {
    /// Load and parse an ELF binary from file path
    pub fn load(path: impl AsRef<Path>) -> Result<LoadedBinary> {
        let path_ref = path.as_ref();
        let path_str = path_ref.to_str().ok_or_else(|| anyhow!("Invalid path"))?;

        info!("Loading ELF binary: {}", path_str);

        // Read file into buffer
        let mut file = File::open(path_ref)
            .with_context(|| format!("Failed to open ELF file: {}", path_str))?;

        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)
            .context("Failed to read ELF file")?;

        Self::load_from_buffer(&buffer, path_str)
    }

    /// Load and parse ELF binary from memory buffer
    pub fn load_from_buffer(buffer: &[u8], path: &str) -> Result<LoadedBinary> {
        // Parse ELF header and structure
        let elf = Elf::parse(buffer).context("Failed to parse ELF binary")?;

        debug!(
            "ELF Header: machine={:?}, type={:?}, entry=0x{:x}",
            elf.header.e_machine, elf.header.e_type, elf.header.e_entry
        );

        // Validate ELF
        Self::validate_elf(&elf)?;

        // Determine if PIE
        let is_pie = elf.header.e_type == ET_DYN;
        debug!(
            "Binary type: {}",
            if is_pie {
                "PIE (ET_DYN)"
            } else {
                "Executable (ET_EXEC)"
            }
        );

        // Extract components
        let segments = Self::extract_segments(&elf);
        let sections = Self::extract_sections(&elf);
        let symbols = Self::extract_symbols(&elf);
        let relocations = Self::extract_relocations(&elf, buffer)?;
        let dynamic_info = Self::extract_dynamic_info(&elf, buffer)?;

        // Calculate load bias for PIE
        let load_bias = if is_pie {
            0 // Will be set at runtime based on where binary is loaded
        } else {
            segments
                .iter()
                .filter(|s| s.segment_type == PT_LOAD)
                .next()
                .map(|s| s.virtual_address)
                .unwrap_or(0)
        };

        let binary = LoadedBinary {
            path: path.to_string(),
            entry_point: elf.header.e_entry,
            is_pie,
            load_bias,
            segments,
            sections,
            symbols,
            relocations,
            dynamic_info,
        };

        info!(
            "✓ Successfully loaded ELF binary: {}",
            if is_pie { "PIE" } else { "Executable" }
        );
        info!("  Entry point: 0x{:x}", binary.entry_point);
        info!("  Segments: {}", binary.segments.len());
        info!("  Sections: {}", binary.sections.len());
        info!("  Symbols: {}", binary.symbols.len());
        info!("  Relocations: {}", binary.relocations.len());

        Ok(binary)
    }

    /// Validate ELF header and architecture
    fn validate_elf(elf: &Elf) -> Result<()> {
        // Check file type
        match elf.header.e_type {
            ET_EXEC => debug!("Binary type: Executable"),
            ET_DYN => debug!("Binary type: Shared Object/PIE"),
            _ => return Err(anyhow!("Unsupported ELF type: {}", elf.header.e_type)),
        }

        // Check architecture support
        match elf.header.e_machine {
            goblin::elf::header::EM_X86_64 => debug!("Architecture: x86-64"),
            goblin::elf::header::EM_386 => debug!("Architecture: x86-32"),
            goblin::elf::header::EM_ARM => debug!("Architecture: ARM"),
            goblin::elf::header::EM_AARCH64 => debug!("Architecture: ARM64"),
            arch => {
                warn!("Unsupported architecture: {}", arch);
                return Err(anyhow!("Unsupported architecture: {}", arch));
            }
        }

        Ok(())
    }

    /// Extract loadable segments (PT_LOAD)
    fn extract_segments(elf: &Elf) -> Vec<Segment> {
        elf.program_headers
            .iter()
            .map(|ph| Segment {
                segment_type: ph.p_type,
                virtual_address: ph.p_vaddr,
                physical_address: ph.p_paddr,
                file_offset: ph.p_offset,
                file_size: ph.p_filesz,
                memory_size: ph.p_memsz,
                flags: ph.p_flags,
                alignment: ph.p_align,
            })
            .collect()
    }

    /// Extract sections from section header table
    fn extract_sections(elf: &Elf) -> Vec<Section> {
        elf.section_headers
            .iter()
            .filter_map(|sh| {
                let name = elf.shdr_strtab.get_at(sh.sh_name);
                name.map(|n| Section {
                    name: n.to_string(),
                    section_type: sh.sh_type,
                    virtual_address: sh.sh_addr,
                    file_offset: sh.sh_offset,
                    size: sh.sh_size,
                    alignment: sh.sh_addralign,
                    flags: sh.sh_flags,
                })
            })
            .collect()
    }

    /// Extract symbol table entries
    fn extract_symbols(elf: &Elf) -> Vec<Symbol> {
        elf.syms
            .iter()
            .enumerate()
            .filter_map(|(idx, sym)| {
                // Skip null symbols
                if sym.st_name == 0 && idx != 0 {
                    return None;
                }

                let name = elf.strtab.get_at(sym.st_name).unwrap_or("").to_string();

                let binding = match sym.st_bind() {
                    0 => SymbolBinding::Local,
                    1 => SymbolBinding::Global,
                    2 => SymbolBinding::Weak,
                    other => SymbolBinding::Other(other as u8),
                };

                let symbol_type = match sym.st_type() {
                    0 => SymbolType::NoType,
                    1 => SymbolType::Object,
                    2 => SymbolType::Function,
                    3 => SymbolType::Section,
                    4 => SymbolType::File,
                    other => SymbolType::Other(other as u8),
                };

                let visibility = match sym.st_visibility() {
                    0 => SymbolVisibility::Default,
                    1 => SymbolVisibility::Internal,
                    2 => SymbolVisibility::Hidden,
                    3 => SymbolVisibility::Protected,
                    _ => SymbolVisibility::Default,
                };

                Some(Symbol {
                    name,
                    value: sym.st_value,
                    size: sym.st_size as u32,
                    binding,
                    symbol_type,
                    visibility,
                    section_index: sym.st_shndx as u16,
                })
            })
            .collect()
    }

    /// Extract relocation entries
    fn extract_relocations(elf: &Elf, buffer: &[u8]) -> Result<Vec<Relocation>> {
        let mut relocations = Vec::new();

        // Process RELA relocations (.rela.dyn, .rela.plt)
        for (name, sh) in elf.section_headers.iter().enumerate() {
            if sh.sh_type != SHT_RELA {
                continue;
            }

            let section_name = elf.shdr_strtab.get_at(sh.sh_name);

            debug!(
                "Processing relocation section: {}",
                section_name.unwrap().to_string()
            );

            // Parse RELA entries
            let rela_data = &buffer[sh.sh_offset as usize..(sh.sh_offset + sh.sh_size) as usize];

            // Each RELA entry is 24 bytes on 64-bit
            for i in (0..rela_data.len()).step_by(24) {
                if i + 24 > rela_data.len() {
                    break;
                }

                let offset = u64::from_le_bytes(rela_data[i..i + 8].try_into()?);
                let info = u64::from_le_bytes(rela_data[i + 8..i + 16].try_into()?);
                let addend = i64::from_le_bytes(rela_data[i + 16..i + 24].try_into()?);

                let symbol_index = (info >> 32) as u32;
                let relocation_type = (info & 0xffffffff) as u32;

                relocations.push(Relocation {
                    offset,
                    symbol_index,
                    relocation_type,
                    addend: Some(addend),
                });
            }
        }

        Ok(relocations)
    }

    /// Extract dynamic linking information
    fn extract_dynamic_info(elf: &Elf, buffer: &[u8]) -> Result<Option<DynamicInfo>> {
        // Only for dynamically linked binaries
        if elf.dynamic.is_none() {
            return Ok(None);
        }

        let mut info = DynamicInfo {
            interpreter: None,
            needed_libraries: Vec::new(),
            rpath: Vec::new(),
            runpath: Vec::new(),
            soname: None,
            gnu_hash_offset: None,
            hash_offset: None,
            symtab_offset: None,
            strtab_offset: None,
        };

        // Extract interpreter from PT_INTERP
        for ph in &elf.program_headers {
            if ph.p_type == PT_INTERP {
                let interp_data =
                    &buffer[ph.p_offset as usize..(ph.p_offset + ph.p_filesz) as usize];
                let interp_str = String::from_utf8_lossy(interp_data)
                    .trim_end_matches('\0')
                    .to_string();
                info.interpreter = Some(interp_str.clone());
                debug!("Found interpreter: {}", interp_str);
                break;
            }
        }

        // Extract dynamic section entries
        if let Some(dyn_table) = &elf.dynamic {
            for dyn_entry in dyn_table.dyns.iter() {
                match dyn_entry.d_tag {
                    goblin::elf::dynamic::DT_NEEDED => {
                        if let Some(lib_name) = elf.dynstrtab.get_at(dyn_entry.d_val as usize) {
                            if let name = lib_name {
                                info.needed_libraries.push(name.to_string());
                                debug!("Found needed library: {}", name);
                            }
                        }
                    }
                    goblin::elf::dynamic::DT_RPATH => {
                        if let Some(rpath_result) = elf.dynstrtab.get_at(dyn_entry.d_val as usize) {
                            if let path_string = rpath_result {
                                info.rpath =
                                    path_string.split(':').map(|s| s.to_string()).collect();
                            }
                        }
                    }
                    goblin::elf::dynamic::DT_RUNPATH => {
                        if let Some(runpath_result) = elf.dynstrtab.get_at(dyn_entry.d_val as usize)
                        {
                            if let path_string = runpath_result {
                                info.runpath =
                                    path_string.split(':').map(|s| s.to_string()).collect();
                            }
                        }
                    }

                    goblin::elf::dynamic::DT_SONAME => {
                        if let Some(soname) = elf.dynstrtab.get_at(dyn_entry.d_val as usize) {
                            if let name = soname {
                                info.soname = Some(name.to_string());
                            }
                        }
                    }
                    goblin::elf::dynamic::DT_GNU_HASH => {
                        info.gnu_hash_offset = Some(dyn_entry.d_val);
                    }
                    goblin::elf::dynamic::DT_HASH => {
                        info.hash_offset = Some(dyn_entry.d_val);
                    }
                    goblin::elf::dynamic::DT_SYMTAB => {
                        info.symtab_offset = Some(dyn_entry.d_val);
                    }
                    goblin::elf::dynamic::DT_STRTAB => {
                        info.strtab_offset = Some(dyn_entry.d_val);
                    }
                    _ => {}
                }
            }
        }

        Ok(Some(info))
    }

    /// Calculate load bias for PIE binaries
    ///
    /// Load bias is the difference between runtime base and link-time base
    /// For PIE: load_bias = runtime_base - 0 (link-time base is 0)
    /// For ET_EXEC: load_bias = 0 (uses fixed addresses)
    pub fn calculate_load_bias(binary: &LoadedBinary, runtime_base: u64) -> u64 {
        if binary.is_pie {
            runtime_base
        } else {
            0
        }
    }

    /// Resolve a symbol by name (static lookup only)
    pub fn resolve_symbol(binary: &LoadedBinary, name: &str) -> Option<Symbol> {
        binary.symbols.iter().find(|sym| sym.name == name).cloned()
    }

    /// Get all exported symbols (global, visible)
    pub fn get_exported_symbols(binary: &LoadedBinary) -> Vec<&Symbol> {
        binary
            .symbols
            .iter()
            .filter(|sym| {
                sym.binding == SymbolBinding::Global
                    && sym.visibility == SymbolVisibility::Default
                    && !sym.name.is_empty()
            })
            .collect()
    }

    /// Get relocation info for address
    pub fn get_relocations_at(binary: &LoadedBinary, address: u64) -> Vec<&Relocation> {
        binary
            .relocations
            .iter()
            .filter(|r| r.offset == address)
            .collect()
    }

    /// Get memory layout info
    pub fn get_memory_layout(binary: &LoadedBinary) -> MemoryLayout {
        let loadable_segments: Vec<_> = binary
            .segments
            .iter()
            .filter(|s| s.segment_type == PT_LOAD)
            .collect();

        let min_addr = loadable_segments
            .iter()
            .map(|s| s.virtual_address)
            .min()
            .unwrap_or(0);

        let max_addr = loadable_segments
            .iter()
            .map(|s| s.virtual_address + s.memory_size)
            .max()
            .unwrap_or(0);

        let total_size = max_addr - min_addr;

        MemoryLayout {
            min_address: min_addr,
            max_address: max_addr,
            total_size,
            loadable_segments: loadable_segments.len(),
        }
    }
}

/// Memory layout information
#[derive(Debug)]
pub struct MemoryLayout {
    pub min_address: u64,
    pub max_address: u64,
    pub total_size: u64,
    pub loadable_segments: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_binding() {
        assert_eq!(SymbolBinding::Local, SymbolBinding::Local);
        assert_ne!(SymbolBinding::Local, SymbolBinding::Global);
    }

    #[test]
    fn test_symbol_types() {
        assert_eq!(SymbolType::Function, SymbolType::Function);
        assert_ne!(SymbolType::Function, SymbolType::Object);
    }

    #[test]
    fn test_load_bias_calculation() {
        let mut binary = LoadedBinary {
            path: "test".to_string(),
            entry_point: 0x400000,
            is_pie: true,
            load_bias: 0,
            segments: Vec::new(),
            sections: Vec::new(),
            symbols: Vec::new(),
            relocations: Vec::new(),
            dynamic_info: None,
        };

        // PIE: load bias should equal runtime base
        let bias = ElfLoader::calculate_load_bias(&binary, 0x7f0000000000);
        assert_eq!(bias, 0x7f0000000000);

        // Non-PIE: load bias should be 0
        binary.is_pie = false;
        let bias = ElfLoader::calculate_load_bias(&binary, 0x7f0000000000);
        assert_eq!(bias, 0);
    }
}
