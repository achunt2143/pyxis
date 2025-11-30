// ============================================================================
// DOCUMENT SERVICE MODULE
// File: gaia/src/document_service.rs
// Purpose: Document editing and management
// ============================================================================

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Document {
    pub id: String,
    pub title: String,
    pub content: String,
    pub format: DocumentFormat,
    pub modified: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum DocumentFormat {
    Text,
    Markdown,
    Html,
    Pdf,
}

pub struct DocumentService {
    documents: Vec<Document>,
}

impl DocumentService {
    pub fn new() -> Self {
        info!("📄 Initializing Document Service");
        
        DocumentService {
            documents: vec![],
        }
    }

    pub fn create_document(&mut self, title: &str, format: DocumentFormat) -> Document {
        debug!("✨ Creating new document: {}", title);
        
        let doc = Document {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.to_string(),
            content: String::new(),
            format,
            modified: chrono::Local::now().to_string(),
        };
        
        self.documents.push(doc.clone());
        doc
    }

    pub fn save_document(&mut self, id: &str, content: &str) -> Result<(), String> {
        debug!("💾 Saving document: {}", id);
        
        if let Some(doc) = self.documents.iter_mut().find(|d| d.id == id) {
            doc.content = content.to_string();
            doc.modified = chrono::Local::now().to_string();
            Ok(())
        } else {
            Err("Document not found".to_string())
        }
    }

    pub fn get_document(&self, id: &str) -> Option<Document> {
        self.documents.iter().find(|d| d.id == id).cloned()
    }

    pub fn list_documents(&self) -> Vec<Document> {
        self.documents.clone()
    }
}