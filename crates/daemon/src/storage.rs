use super::*;

impl Service {
    pub async fn collect_storage_once(&self) -> Result<()> {
        let _collector = self.storage_collection.lock().await;
        let result = self.backend.request(HostRequest::Storage).await;
        match result {
            Ok(HostResponse::Storage(inventory)) => {
                *self.storage.write().await = inventory;
                Ok(())
            }
            response => {
                let error = match response {
                    Err(error) => error,
                    _ => anyhow::anyhow!("Unexpected storage adapter response"),
                };
                let mut inventory = self.storage.write().await;
                let now = homecloud_store::now();
                let StorageInventory {
                    disk_collection,
                    pool_collection,
                    ..
                } = &mut *inventory;
                for collection in [disk_collection, pool_collection] {
                    collection.attempted_at = Some(now);
                    collection.error = Some(error.to_string());
                }
                Err(error)
            }
        }
    }
}
