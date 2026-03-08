use crate::boot_params::BootParams;
use crate::error::{BootError, Result};
use crate::layout::{CMDLINE_ADDR, CMDLINE_MAX_SIZE};
use nova_mem::{GuestAddress, GuestMemoryMmap};

/// Builder for the kernel command line.
pub struct CmdlineBuilder {
    parts: Vec<String>,
}

impl CmdlineBuilder {
    /// Create a new empty command line builder.
    pub fn new() -> Self {
        Self { parts: Vec::new() }
    }

    /// Append a key=value parameter.
    pub fn arg(mut self, key: &str, value: &str) -> Self {
        self.parts.push(format!("{key}={value}"));
        self
    }

    /// Append a bare flag (no value).
    pub fn flag(mut self, flag: &str) -> Self {
        self.parts.push(flag.to_string());
        self
    }

    /// Append a raw string.
    pub fn raw(mut self, s: &str) -> Self {
        self.parts.push(s.to_string());
        self
    }

    /// Build the command line string.
    pub fn build(&self) -> String {
        self.parts.join(" ")
    }

    /// Build and write the command line to guest memory, updating boot_params.
    pub fn write_to_memory(
        &self,
        mem: &GuestMemoryMmap,
        boot_params: &mut BootParams,
    ) -> Result<()> {
        let cmdline = self.build();
        let bytes = cmdline.as_bytes();

        if bytes.len() >= CMDLINE_MAX_SIZE {
            return Err(BootError::CmdlineTooLong {
                len: bytes.len(),
                max: CMDLINE_MAX_SIZE,
            });
        }

        // Write the command line string + null terminator.
        let mut buf = Vec::with_capacity(bytes.len() + 1);
        buf.extend_from_slice(bytes);
        buf.push(0); // null terminator

        mem.write_slice(GuestAddress::new(CMDLINE_ADDR), &buf)?;

        // Update boot_params to point to the command line.
        let header = boot_params.setup_header_mut();
        header.cmd_line_ptr = CMDLINE_ADDR as u32;
        header.cmdline_size = bytes.len() as u32;

        tracing::info!(
            cmdline = %cmdline,
            addr = CMDLINE_ADDR,
            "wrote kernel command line"
        );

        Ok(())
    }
}

impl Default for CmdlineBuilder {
    fn default() -> Self {
        Self::new()
    }
}
