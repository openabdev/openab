use super::{ContextFetchRequest, ContextMessage, ContextObserveRequest, ContextProvider};

#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub struct ApiFetchContextProvider;

#[async_trait::async_trait]
impl ContextProvider for ApiFetchContextProvider {
    fn is_enabled(&self) -> bool {
        false
    }

    async fn observe(&self, _request: ContextObserveRequest) -> bool {
        false
    }

    async fn fetch_context(&self, _request: ContextFetchRequest) -> Option<Vec<ContextMessage>> {
        None
    }
}
