//! Needle3 的 Rig 0.44 模型面：`Needle3Model` = `Model<Needle3Wire, Needle3Transport>`。
//!
//! ```text
//! CompletionRequest ──encode──▶ NeedlePayload（会话输入）
//!        │                              │
//!        │                     Transport（Needle worker 阻塞解码）
//!        ▼                              ▼
//!    Completion ◀──Turn fold── NeedleFrame（单帧信封）
//! ```
//!
//! Needle 一次 complete 产出一个 JSON 信封（无 token 流），因此本 wire 的
//! 回复永远单帧：`Mode::Unary` 与 `Mode::Streaming` 编解码同一信封——
//! 流式请求得到一个只含终点的流（上游 `Local` wire 同样只产 whole reply）。
//!
//! 边界（I1/I22）：`Transport::send` 不做解码；它把作业交给
//! [`super::worker::Needle3Worker`] 并登记回灌槽。`Opening` 的 future 只在
//! 被轮询时等待 worker 落回帧——ECS 主线程只 poll，永不 block_on；真正的
//! `needle_complete()` 在 worker 线程。

use std::future::Future;
use std::sync::Arc;

use rig_core::completion::message::{Message, ToolResultContent};
use rig_core::completion::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderCapabilities, Usage,
};
use rig_core::driver::{Exchange, Model, Opening, Opened, Transport};
use rig_core::error::{EncodeError, ProviderError};
use rig_core::message::{CallId, ToolName};
use rig_core::operation::{Block, Completion, Finish};
use rig_core::streaming::Streamed;
use rig_core::wasm_compat::WasmCompatSend;
use rig_core::wire::{
    Capabilities, Decoder, Descriptor, Flow, Mode, Out, Wire, WireEvent,
    document::{Reassemble, Serves},
};
use serde_json::json;

use crate::engine::NeedleResponse;

use super::codec;
use super::worker::{Needle3Worker, Submitted};

/// Needle 事件：单帧回复里分类出的信封（本 wire 唯一的事件类型）。
#[derive(Debug, Clone)]
pub struct NeedleEvent(pub(crate) NeedleResponse);

/// Needle 帧：单帧信封的 JSON 文本。
#[derive(Debug, Clone)]
pub struct NeedleFrame(pub(crate) String);

/// Needle completion wire：`Wire<Op = Completion>` 的本地实现。
///
/// `describe()` 自报 `"needle"` provider 与完成能力（无 reasoning、无原生
/// structured output、无媒体；工具调用支持）。`ReplayTarget` 每个选项都答
/// `Nothing`（unset）或 `Unsupported`（set）——Needle 没有采样/缓存选项面；
/// `states_finish_reason()` 为 `false`（本地 wire 无 finish 词表，pi 规则）。
#[derive(Clone, Debug, Default)]
pub struct Needle3Wire {
    /// 引擎代际标签（`describe().model`；诊断面）。
    label: Arc<str>,
    /// 规范 call_id 序列（I18 v27）：model 实例级单调计数——Needle 从不发
    /// call id，本 wire 的解码器按此生成 `needle-call-<seq>`，生成一次、
    /// 两侧同值、跨回复/跨 run 不碰撞（run 级唯一的实现形态）。
    /// Clone 共享同一计数器（Rig 每次 call 都 clone wire）。
    call_seq: Arc<std::sync::atomic::AtomicU64>,
}

impl Needle3Wire {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: Arc::from(label.into().as_str()),
            call_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }
}

impl rig_core::completion::ReplayTarget for Needle3Wire {
    fn api(&self) -> rig_core::message::Api {
        rig_core::message::Api::from_static("needle.complete")
    }

    fn map_options(
        &self,
        _request: &CompletionRequest,
        fields: rig_core::completion::options::OptionFields<'_>,
    ) -> rig_core::completion::options::OptionMap {
        use rig_core::completion::options::{Mapping, OptionFields, OptionMap};
        let OptionFields {
            reasoning,
            cache,
            service_tier,
            verbosity,
            parallel_tool_calls: _,
            top_p,
            seed,
            stop,
        } = fields;
        let refused = |set: bool, what: &str| match set {
            true => Mapping::Unsupported(format!("needle3 does not take {what}")),
            false => Mapping::Nothing,
        };
        OptionMap {
            reasoning: refused(reasoning.is_some(), "reasoning"),
            cache: refused(cache.is_some(), "cache retention"),
            service_tier: refused(service_tier.is_some(), "a service tier"),
            verbosity: refused(verbosity.is_some(), "verbosity"),
            parallel_tool_calls: Mapping::Nothing,
            top_p: refused(top_p.is_some(), "top_p"),
            seed: refused(seed.is_some(), "a seed"),
            stop: refused(!stop.is_empty(), "stop sequences"),
        }
    }

    fn provider(&self) -> &str {
        "needle"
    }

    fn model(&self) -> &str {
        &self.label
    }

    fn accepts(&self, _model: &str) -> rig_core::completion::Accepts {
        // Needle 只读文本与工具调用：图片/音频/视频一律占位降级（adapt 的规则）。
        rig_core::completion::Accepts::TEXT
    }

    /// 本地 wire 无 finish 词表（pi 规则：说 `false`）。
    fn states_finish_reason(&self) -> bool {
        false
    }
}

impl Wire for Needle3Wire {
    type Op = Completion;
    type Payload = codec::NeedlePayload;
    type Frame = NeedleFrame;
    type Decoder<'id> = NeedleDecoder;
    type Reassembler = NeedleDocument;

    fn describe(&self) -> Descriptor<'_> {
        Descriptor::new("needle")
            .model(self.label.as_ref())
            .capabilities(Capabilities::completion(
                ProviderCapabilities::default().with_native_output_tool_composition(false),
            ))
            .replay(self)
    }

    fn encode(
        &self,
        request: CompletionRequest,
        _mode: Mode,
    ) -> Result<Self::Payload, EncodeError> {
        codec::encode_payload(&request)
    }

    fn decoder<'id>(&self) -> Self::Decoder<'id> {
        NeedleDecoder {
            call_seq: Arc::clone(&self.call_seq),
        }
    }

    fn reassembler(&self) -> Self::Reassembler {
        NeedleDocument::default()
    }
}

/// Needle 解码器：单帧信封 → completion writer。
///
/// 持 model 实例级 call_id 计数器（I18 v27：单一 mint 点在
/// [`super::codec::call_id`]，两侧同值）。
#[derive(Debug, Default, Clone)]
pub struct NeedleDecoder {
    call_seq: Arc<std::sync::atomic::AtomicU64>,
}

impl<'id> Decoder<'id, Completion, NeedleFrame> for NeedleDecoder {
    type Event = NeedleEvent;

    fn classify(&self, frame: NeedleFrame) -> WireEvent<Self::Event> {
        match serde_json::from_str::<NeedleResponse>(&frame.0) {
            Ok(response) => WireEvent::Known(NeedleEvent(response)),
            Err(error) => WireEvent::Corrupt(error),
        }
    }

    fn decode(
        &mut self,
        event: Self::Event,
        mut out: Out<'id, Completion>,
    ) -> Result<Flow, ProviderError> {
        let response = event.0;
        if let Some(error) = &response.error {
            return Err(ProviderError::Response(error.clone()));
        }
        if response.is_call() {
            for (index, call) in response.function_calls.iter().enumerate() {
                let name = ToolName::new(call.name.as_str())
                    .map_err(|_| ProviderError::Response("needle tool call has an empty name".into()))?;
                let arguments = serde_json::to_value(&call.arguments)
                    .map_err(|err| ProviderError::Response(format!("needle call arguments: {err}")))?;
                // `Out::whole`：单步开-写-收；provider item 为 Null（无原生项）。
                // call id 经 codec::call_id 单点 mint（I18 v27）。
                out.whole(
                    index,
                    Block::Call {
                        id: CallId::from_wire(super::codec::call_id(&self.call_seq)),
                        name,
                    },
                    serde_json::Value::Null,
                    &arguments.to_string(),
                )?;
            }
        } else {
            let text = response.summary();
            if !text.is_empty() {
                out.whole(0, Block::Text, serde_json::Value::Null, &text)?;
            }
        }
        Ok(out.end(Finish::default()))
    }
}

/// Needle 的 raw 重建器：单帧信封 → 整个信封 JSON（`raw` 字段，§15）。
#[derive(Debug, Default)]
pub struct NeedleDocument {
    frame: Option<String>,
}

impl Serves<Completion> for NeedleDocument {}

impl Reassemble<NeedleFrame> for NeedleDocument {
    fn absorb(&mut self, frame: &NeedleFrame) {
        self.frame = Some(frame.0.clone());
    }

    fn finish(self) -> serde_json::Value {
        self.frame
            .and_then(|frame| serde_json::from_str(&frame).ok())
            .unwrap_or(serde_json::Value::Null)
    }
}

/// Needle3 完成模型：`Model<Needle3Wire, Needle3Transport>`（Rig 0.44 契约）。
///
/// 构造即共享 worker（I7：模型生命周期由本类型持有；I22：解码在 worker）。
#[derive(Clone)]
pub struct Needle3Model {
    inner: Model<Needle3Wire, Needle3Transport>,
}impl std::fmt::Debug for Needle3Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Needle3Model")
            .field("provider", &self.inner.name())
            .field("model", &self.inner.id())
            .finish()
    }
}

impl Needle3Model {
    /// 把 `worker` 下的一个 Needle 会话接成 Rig 模型。
    ///
    /// `label` 是 `describe().model`（诊断与 route 键语义）。
    pub fn new(worker: Arc<Needle3Worker>, label: impl Into<String>) -> Self {
        Self {
            inner: Model::new(
                Needle3Wire::new(label),
                Needle3Transport { worker },
            ),
        }
    }

    /// unary 完成（Rig 0.44 `Model::call` 语义）。
    pub fn call(
        &self,
        request: impl Into<CompletionRequest>,
    ) -> impl Future<Output = Result<CompletionResponse, ProviderError>> + WasmCompatSend + 'static {
        self.inner.call(request)
    }

    /// 流式完成（Needle 无 token 流：单帧降级为一个只含终点的流）。
    pub fn stream(
        &self,
        request: impl Into<CompletionRequest>,
    ) -> Result<Streamed<Completion>, ProviderError> {
        self.inner.stream(request)
    }
}

impl Needle3Model {
    /// 内部 `Model`（rig-ecs 注册路径擦成 `DynModel` 用；`rig-ecs` feature）。
    #[cfg(feature = "rig-ecs")]
    pub fn into_inner(self) -> Model<Needle3Wire, Needle3Transport> {
        self.inner
    }
}

/// Needle transport：payload → worker 作业票，帧由 worker 落回（I22）。
#[derive(Clone)]
pub struct Needle3Transport {
    worker: Arc<Needle3Worker>,
}

impl Transport<Needle3Wire> for Needle3Transport {
    fn send(&self, payload: codec::NeedlePayload, _exchange: Exchange) -> Opening<NeedleFrame> {
        let Submitted { wait, .. } = self.worker.submit(payload);
        // worker 落回信封 → 单帧 JSON 文本；错误回执 → 帧失败（I27）。
        Opening::new(async move {
            let response = wait.await;
            response
                .map(|envelope| {
                    NeedleFrame(serde_json::to_string(&envelope).unwrap_or_else(|_| "null".into()))
                })
                .map_err(|err| ProviderError::Response(err.to_string()))
                .map(|frame| Opened::new(single_frame(frame)))
        })
    }
}

/// 单帧流：一个 Ready 的帧（Needle 无分帧；上游 `Local` wire 同形）。
fn single_frame(
    frame: NeedleFrame,
) -> rig_core::wasm_compat::WasmBoxedStream<'static, Result<NeedleFrame, ProviderError>> {
    Box::pin(futures::stream::once(std::future::ready(Ok(frame))))
}

/// Needle3 信封 → `CompletionResponse` 的独立转换（测试/诊断用）。
///
/// `raw` 是信封原文（§15：confidence/reasoning/tps 全在 `raw` 可取）；
/// `origin` 报 `needle` provider；usage 为空（引擎不报 token）。
pub fn envelope_to_completion(response: NeedleResponse) -> CompletionResponse {
    let seq = std::sync::atomic::AtomicU64::new(0);
    let choice = codec::choice_from_envelope(&response, &seq);
    let raw = serde_json::to_value(&response).unwrap_or(json!({}));
    let mut origin = rig_core::message::Origin::new("needle.complete", "needle", "needle3");
    origin.response_id = None;
    let mut completion = CompletionResponse::new(choice, Usage::default(), origin, raw);
    if response.is_call() {
        completion = completion.with_finish_reason(FinishReason::ToolCalls);
    }
    completion
}

/// Needle 工具结果回喂 → 会话输入的官方语义数组（测试用）。
pub fn tool_results_json(results: &[ToolResultContent]) -> Result<String, serde_json::Error> {
    codec::tool_results_input(results)
}

/// needle 会话输入的历史尾部判定（测试用；§10）。
pub fn terminal_user_input(history: &[Message]) -> Option<String> {
    codec::terminal_input(history)
}

/// `Usage::default()` 的公开别名（测试断言用）。
pub fn empty_usage() -> Usage {
    Usage::default()
}
