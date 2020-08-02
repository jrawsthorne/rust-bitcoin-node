#[derive(Debug, Clone, Copy)]
pub struct FileRecord {
    pub blocks: u32,
    pub used: u32,
    pub length: u32,
}

impl FileRecord {
    pub fn new(blocks: u32, used: u32, length: u32) -> Self {
        Self {
            blocks,
            used,
            length,
        }
    }

    pub fn empty(length: u32) -> Self {
        Self {
            blocks: 0,
            used: 0,
            length,
        }
    }
}

impl Default for FileRecord {
    fn default() -> Self {
        Self {
            blocks: 0,
            used: 0,
            length: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BlockRecord {
    pub file: u32,
    pub position: u32,
    pub length: u32,
}

impl Default for BlockRecord {
    fn default() -> Self {
        Self {
            file: 0,
            position: 0,
            length: 0,
        }
    }
}

impl BlockRecord {
    pub fn new(file: u32, position: u32, length: u32) -> Self {
        Self {
            file,
            position,
            length,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RecordType {
    Block,
    Undo,
}

impl RecordType {
    pub fn prefix(&self) -> String {
        match self {
            RecordType::Block => String::from("blk"),
            RecordType::Undo => String::from("blu"),
        }
    }

    pub fn as_u32(&self) -> u32 {
        match self {
            RecordType::Block => 0,
            RecordType::Undo => 1,
        }
    }
}
