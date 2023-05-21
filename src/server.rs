use crate::index::{IndexManager, Text};
use crate::text_splitters::TextSplitterKind;
use crate::{CreateIndexParams, EmbeddingRouter, ConversationHistoryRouter, MetricKind, ServerConfig};

use super::embeddings::EmbeddingGenerator;
use super::memory::ConversationHistory;
use anyhow::Result;
use axum::http::StatusCode;
use axum::{extract::State, routing::get, routing::post, Json, Router};
use tracing::info;

use serde::{Deserialize, Serialize};
use smart_default::SmartDefault;
use std::collections::HashMap;

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

/// Request payload for generating text embeddings.
#[derive(Debug, Serialize, Deserialize)]
struct GenerateEmbeddingRequest {
    /// Input texts for which embeddings will be generated.
    inputs: Vec<String>,
    /// Name of the model to use for generating embeddings.
    model: String,
}

/// Response payload for generating text embeddings.
#[derive(Debug, Serialize, Deserialize)]
struct GenerateEmbeddingResponse {
    /// Generated embeddings, if successful.
    embeddings: Option<Vec<Vec<f32>>>,
    /// Error message, if an error occurred.
    error: Option<String>,
}

/// An embedding model and its properties.
#[derive(Debug, Serialize, Deserialize)]
struct EmbeddingModel {
    /// Name of the embedding model.
    name: String,
    /// Number of dimensions in the embeddings generated by this model.
    dimensions: u64,
}

/// Response payload for listing available embedding models.
#[derive(Debug, Serialize, Deserialize)]
struct ListEmbeddingModelsResponse {
    /// List of available embedding models.
    models: Vec<EmbeddingModel>,
}

#[derive(SmartDefault, Debug, Serialize, Deserialize, strum::Display)]
#[strum(serialize_all = "snake_case")]
enum ApiTextSplitterKind {
    // Do not split text.
    #[serde(rename = "none")]
    None,

    /// Split text by new lines.
    #[default]
    #[serde(rename = "new_line")]
    NewLine,

    /// Split a document across the regex boundary
    #[serde(rename = "regex")]
    Regex { pattern: String },
}

#[derive(SmartDefault, Debug, Serialize, Deserialize)]
enum  MemoryPolicy {
    // Use Simple policy
    #[default]
    #[serde(rename = "simple")]
    Simple,

    // Use Windows
    #[serde(rename = "window")]
    Window,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename = "metric")]
enum IndexMetric {
    #[serde(rename = "dot")]
    Dot,

    #[serde(rename = "cosine")]
    Cosine,

    #[serde(rename = "euclidean")]
    Euclidean,
}

/// Request payload for creating a new vector index.
#[derive(Debug, Serialize, Deserialize)]
struct IndexCreateRequest {
    /// Name of the new vector index.
    name: String,
    /// Name of the embedding model to use for indexing.
    embedding_model: String,
    /// Number of dimensions in the embeddings generated by the embedding model.
    metric: IndexMetric,
    /// The text splitter to use for splitting text into fragments.
    text_splitter: ApiTextSplitterKind,

    /// Hash on these paramters
    hash_on: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct IndexCreateResponse {
    errors: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Document {
    pub text: String,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AddTextsRequest {
    index: String,
    documents: Vec<Document>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct IndexAdditionResponse {
    errors: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SearchRequest {
    index: String,
    query: String,
    k: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct ConversationHistoryCreateRequest {
    /// The memory policy for storing and retrieving from conversation history.
    memory_policy: MemoryPolicy,
}

struct ConversationHistoryCreateResponse {
    errors: Vec<String>,
}


#[derive(Debug, Serialize, Deserialize, Default)]
struct DocumentFragment {
    text: String,
    metadata: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct IndexSearchResponse {
    results: Vec<DocumentFragment>,
    errors: Vec<String>,
}

type IndexEndpointState = (Arc<Option<IndexManager>>, Arc<EmbeddingRouter>);

type ConversationHistoryState = (Arc<Option<IndexManager>>, Arc<ConversationHistoryRouter>);

pub struct Server {
    addr: SocketAddr,
    config: Arc<ServerConfig>,
}
impl Server {
    /// Creates a new instance of the `Server`.
    ///
    /// # Arguments
    ///
    /// * `config` - An `Arc` containing the server configuration.
    ///
    /// # Returns
    ///
    /// * A result containing the new `Server` instance if successful, or an error if the address cannot be parsed.
    pub fn new(config: Arc<super::server_config::ServerConfig>) -> Result<Self> {
        let addr: SocketAddr = config.listen_addr.parse()?;
        Ok(Self { addr, config })
    }

    /// Starts the server and begins listening for incoming HTTP requests.
    ///
    /// # Returns
    ///
    /// * A result indicating success or failure of the operation.
    pub async fn run(&self) -> Result<()> {
        let embedding_router = Arc::new(EmbeddingRouter::new(self.config.clone())?);
        let conversation_history_router = Arc::new(ConversationHistoryRouter::new(self.config.clone())?);
        let index_manager = Arc::new(
            IndexManager::new(self.config.index_config.clone(), embedding_router.clone()).await?,
        );
        let app = Router::new()
            .route("/", get(root))
            .route(
                "/embeddings/models",
                get(list_embedding_models).with_state(embedding_router.clone()),
            )
            .route(
                "/embeddings/generate",
                get(generate_embedding).with_state(embedding_router.clone()),
            )
            .route(
                "/index/create",
                post(index_create).with_state((index_manager.clone(), embedding_router.clone())),
            )
            .route(
                "/index/add",
                post(add_texts).with_state((index_manager.clone(), embedding_router.clone())),
            )
            .route(
                "/index/search",
                get(index_search).with_state((index_manager.clone(), embedding_router.clone())),
            );

        info!("server is listening at addr {:?}", &self.addr.to_string());
        axum::Server::bind(&self.addr)
            .serve(app.into_make_service())
            .await?;
        Ok(())
    }
}

/// A basic handler that responds with a static string indicating the name of the server.
/// This handler is typically used as a health check or a simple endpoint to verify that
/// the server is running and responding to requests.
async fn root() -> &'static str {
    "Indexify Server"
}

/// A handler for creating a new vector index in the vector database. This handler is responsible
/// for processing incoming requests to create new vector indices, which are used to store and
/// query vector embeddings. The request payload contains the name of the new index, the name of
/// the embedding model to use for indexing, and the text splitter to use when indexing text.
///
/// # Parameters
///
/// * `State(index)`: The current state of the vector database.
/// * `Json(payload)`: The request payload containing the details for creating the new index.
///
/// # Returns
///
/// * A tuple containing an HTTP status code and a JSON response payload. The response payload
///   contains an empty object, as no additional information is returned for this operation.
#[axum_macros::debug_handler]
async fn index_create(
    State(index_args): State<IndexEndpointState>,
    Json(payload): Json<IndexCreateRequest>,
) -> (StatusCode, Json<IndexCreateResponse>) {
    if index_args.0.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(IndexCreateResponse {
                errors: vec!["server is not configured to have indexes".into()],
            }),
        );
    }
    let try_dim = index_args.1.dimensions(payload.embedding_model.clone());
    if let Err(err) = try_dim {
        return (
            StatusCode::BAD_REQUEST,
            Json(IndexCreateResponse {
                errors: vec![err.to_string()],
            }),
        );
    }
    let index_params = CreateIndexParams {
        name: payload.name.clone(),
        vector_dim: try_dim.unwrap(),
        metric: match payload.metric {
            IndexMetric::Cosine => MetricKind::Cosine,
            IndexMetric::Dot => MetricKind::Dot,
            IndexMetric::Euclidean => MetricKind::Euclidean,
        },
        unique_params: payload.hash_on,
    };
    let index_manager = index_args.0.as_ref();
    let splitter_kind = TextSplitterKind::from_str(&payload.text_splitter.to_string()).unwrap();
    let result = index_manager
        .as_ref()
        .unwrap()
        .create_index(index_params, payload.embedding_model, splitter_kind)
        .await;
    if let Err(err) = result {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(IndexCreateResponse {
                errors: vec![err.to_string()],
            }),
        );
    }
    (StatusCode::OK, Json(IndexCreateResponse { errors: vec![] }))
}

#[axum_macros::debug_handler]
async fn add_texts(
    State(index_args): State<IndexEndpointState>,
    Json(payload): Json<AddTextsRequest>,
) -> (StatusCode, Json<IndexAdditionResponse>) {
    if index_args.0.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(IndexAdditionResponse {
                errors: vec!["server is not configured to have indexes".into()],
            }),
        );
    }
    let index_manager = index_args.0.as_ref().as_ref().unwrap();
    let try_index = index_manager.load(payload.index).await;
    if let Err(err) = try_index {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(IndexAdditionResponse {
                errors: vec![err.to_string()],
            }),
        );
    }
    if try_index.as_ref().unwrap().is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(IndexAdditionResponse {
                errors: vec!["index does not exist".into()],
            }),
        );
    }
    let index = try_index.unwrap().unwrap();
    let texts = payload
        .documents
        .iter()
        .map(|d| Text {
            texts: vec![d.text.to_owned()],
            metadata: d.metadata.to_owned(),
        })
        .collect();
    let result = index.add_texts(texts).await;
    if let Err(err) = result {
        return (
            StatusCode::BAD_REQUEST,
            Json(IndexAdditionResponse {
                errors: vec![err.to_string()],
            }),
        );
    }

    (StatusCode::OK, Json(IndexAdditionResponse::default()))
}

// #[axum_macros::debug_handler]
// async fn create_conversation_history(
//     State(conversation_history_args): State<Arc<dyn ConversationHistory + Sync + Send>>,
//     Json(payload): Json<ConversationHistoryCreateRequest>,
// ) -> (StatusCode, Json<ConversationHistoryCreateResponse>) {

//     let result = ConversationHistory::new(payload.name.clone(), payload.memory_policy.clone()).await;

//     if let Err(err) = result {
//         return (
//             StatusCode::INTERNAL_SERVER_ERROR,
//             Json(ConversationHistoryCreateResponse {
//                 errors: vec![err.to_string()],
//             }),
//         );
//     } else {
//         result
//     }

// }

#[axum_macros::debug_handler]
async fn index_search(
    State(index_args): State<IndexEndpointState>,
    Json(query): Json<SearchRequest>,
) -> (StatusCode, Json<IndexSearchResponse>) {
    if index_args.0.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(IndexSearchResponse {
                errors: vec!["server is not configured to have indexes".into()],
                ..Default::default()
            }),
        );
    }

    let index_manager = index_args.0.as_ref().as_ref().unwrap();
    let try_index = index_manager.load(query.index.clone()).await;
    if let Err(err) = try_index {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(IndexSearchResponse {
                results: vec![],
                errors: vec![err.to_string()],
            }),
        );
    }
    if try_index.as_ref().unwrap().is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(IndexSearchResponse {
                results: vec![],
                errors: vec!["index does not exist".into()],
            }),
        );
    }
    let index = try_index.unwrap().unwrap();
    let results = index.search(query.query, query.k).await;
    if let Err(err) = results {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(IndexSearchResponse {
                results: vec![],
                errors: vec![err.to_string()],
            }),
        );
    }
    let document_fragments: Vec<DocumentFragment> = results
        .unwrap()
        .iter()
        .map(|text| DocumentFragment {
            text: text.texts.to_owned(),
            metadata: text.metadata.to_owned(),
        })
        .collect();
    (
        StatusCode::OK,
        Json(IndexSearchResponse {
            results: document_fragments,
            errors: vec![],
        }),
    )
}

/// A handler for listing the available embedding models supported by the server. This handler
/// retrieves the list of available models from the embedding router and returns it in the response.
/// The response includes the name and dimensions of each available model.
///
/// # Parameters
///
/// * `State(embedding_router)`: The current state of the embedding router.
///
/// # Returns
///
/// * A JSON response payload containing a list of available embedding models and their properties.
#[axum_macros::debug_handler]
async fn list_embedding_models(
    State(embedding_router): State<Arc<EmbeddingRouter>>,
) -> Json<ListEmbeddingModelsResponse> {
    // Retrieve the list of available model names.
    let model_names = embedding_router.list_models();
    let mut models: Vec<EmbeddingModel> = Vec::new();
    // For each model name, retrieve its dimensions and create an EmbeddingModel object.
    for model in model_names {
        if let Ok(dimensions) = embedding_router.dimensions(model.clone()) {
            models.push(EmbeddingModel {
                name: model.clone(),
                dimensions,
            })
        }
    }
    // Return the list of available models in the response.
    Json(ListEmbeddingModelsResponse { models })
}

/// A handler for generating text embeddings using a specified model. This handler processes
/// incoming requests to generate embeddings for a list of input texts. The request payload
/// specifies the input texts and the name of the model to use for generating embeddings.
///
/// # Parameters
///
/// * `State(embedding_generator)`: The current state of the embedding generator.
/// * `Json(payload)`: The request payload containing the input texts and model name.
///
/// # Returns
///
/// * A tuple containing an HTTP status code and a JSON response payload. The response payload
///   contains the generated embeddings if successful, or an error message if an error occurred.
#[axum_macros::debug_handler]
async fn generate_embedding(
    State(embedding_generator): State<Arc<dyn EmbeddingGenerator + Sync + Send>>,
    Json(payload): Json<GenerateEmbeddingRequest>,
) -> (StatusCode, Json<GenerateEmbeddingResponse>) {
    let embeddings = embedding_generator
        .generate_embeddings(payload.inputs, payload.model)
        .await;

    if let Err(err) = embeddings {
        return (
            StatusCode::EXPECTATION_FAILED,
            Json(GenerateEmbeddingResponse {
                embeddings: None,
                error: Some(err.to_string()),
            }),
        );
    }

    (
        StatusCode::OK,
        Json(GenerateEmbeddingResponse {
            embeddings: Some(embeddings.unwrap()),
            error: None,
        }),
    )
}
