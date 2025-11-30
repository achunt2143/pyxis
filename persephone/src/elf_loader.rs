// ELF Loader - Binary Format Handling
// File: persephone/src/elf_loader.rs
// Purpose: Parse and load ELF format binaries

use std::error::Error;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use goblin::elf::Elf;
use tracing::{info, debug};

pub struct ElfLoader;

#[derive(Debug, Clone)]
pub struct LoadedBinary {
    pub path: String,
    pub entry_point: u64,
    pub segments: Vec<Segment>,
    pub symbols: Vec<Symbol>,
    pub dynamic: Option<DynamicInfo>,
}

#[derive(Debug, Clone)]
pub struct Segment {
    pub virtual_address: u64,
    pub file_offset: u64,
    pub file_size: u64,
    pub memory_size: u64,
    pub flags: u32,
    pub alignment: u64,
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub value: u64,
    pub size: u32,
    pub bind: String,
    pub typ: String,
}

#[derive(Debug, Clone)]
pub struct DynamicInfo {
    pub interpreter: String,
    pub needed_libs: Vec<String>,
}

impl ElfLoader {
    /// Load and parse an ELF binary
    pub fn load(path: &str) -> Result<LoadedBinary, Box<dyn Error>> {
        info!("Loading ELF binary: {}", path);
        
        let mut file = File::open(path)?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;
        
        let elf = Elf::parse(&buffer)?;
        
        debug!("ELF Header: e_machine={:?}, e_type={:?}", elf.header.e_machine, elf.header.e_type);
        
        // Validate ELF header
        Self::validate_elf(&elf)?;
        
        // Extract segments
        let segments = Self::extract_segments(&elf);
        
        // Extract symbols
        let symbols = Self::extract_symbols(&elf);
        
        // Extract dynamic information
        let dynamic = Self::extract_dynamic_info(&elf);
        
        let binary = LoadedBinary {
            path: path.to_string(),
            entry_point: elf.header.e_entry,
            segments,
            symbols,
            dynamic,
        };
        
        info!("✓ Successfully loaded ELF binary");
        info!("  Entry point: 0x{:x}", binary.entry_point);
        info!("  Segments: {}", binary.segments.len());
        info!("  Symbols: {}", binary.symbols.len());
        
        Ok(binary)
    }
    
    fn validate_elf(elf: &Elf) -> Result<(), Box<dyn Error>> {
        // Check ELF magic number (already validated by goblin::parse)
        
        // Check if executable or shared object
        match elf.header.e_type {
            goblin::elf::header::ET_EXEC => debug!("Executable binary"),
            goblin::elf::header::ET_DYN => debug!("Shared object/PIE"),
            _ => return Err("Unsupported ELF type".into()),
        }
        
        // Check architecture
        match elf.header.e_machine {
            goblin::elf::header::EM_X86_64 => debug!("x86-64 architecture"),
            goblin::elf::header::EM_386 => debug!("x86 architecture"),
            goblin::elf::header::EM_ARM => debug!("ARM architecture"),
            goblin::elf::header::EM_AARCH64 => debug!("ARM64 architecture"),
            _ => return Err(format!("Unsupported architecture: {}", elf.header.e_machine).into()),
        }
        
        Ok(())
    }
    
    fn extract_segments(elf: &Elf) -> Vec<Segment> {
        elf.program_headers
            .iter()
            .filter(|ph| ph.p_type == goblin::elf::program_header::PT_LOAD)
            .map(|ph| Segment {
                virtual_address: ph.p_vaddr,
                file_offset: ph.p_offset,
                file_size: ph.p_filesz,
                memory_size: ph.p_memsz,
                flags: ph.p_flags,
                alignment: ph.p_align,
            })
            .collect()
    }
    
    fn extract_symbols(elf: &Elf) -> Vec<Symbol> {
        elf.syms
            .iter()
            .filter(|sym| sym.st_name != 0)
            .filter_map(|sym| {
                elf.strtab.get(sym.st_name).ok().and_then(|name| {
                    name.ok().map(|n| Symbol {
                        name: n.to_string(),
                        value: sym.st_value,
                        size: sym.st_size,
                        bind: format!("{:?}", sym.st_bind()),
                        typ: format!("{:?}", sym.st_type()),
                    })
                })
            })
            .collect()
    }
    
    fn extract_dynamic_info(elf: &Elf) -> Option<DynamicInfo> {
        if !elf.dynamic.is_some() {
            return None;
        }
        
        let mut interpreter = String::new();
        let mut needed_libs = Vec::new();
        
        // Extract interpreter path from PT_INTERP
        for ph in &elf.program_headers {
            if ph.p_type == goblin::elf::program_header::PT_INTERP {
                debug!("Found interpreter segment");
            }
        }
        
        // Extract needed libraries from dynamic section
        for dyn_entry in elf.dynamic.as_ref().unwrap() {
            if dyn_entry.d_tag == goblin::elf::dynamic::DT_NEEDED {
                // Would need proper parsing of dynamic string table
                debug!("Found DT_NEEDED entry");
            }
        }
        
        Some(DynamicInfo {
            interpreter,
            needed_libs,
        })
    }
    
    /// Get the load bias for position-independent executables
    pub fn calculate_load_bias(binary: &LoadedBinary) -> u64 {
        binary.segments
            .first()
            .map(|seg| seg.virtual_address)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_elf_loading() {
        // Test with a sample binary (would need actual binary)
    }
}
