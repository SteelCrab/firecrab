use serde_json::Value;
use std::{collections::BTreeMap, fmt};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{AbortController, Request, RequestInit, Response};

#[derive(Debug)]
pub struct Failure {
    pub message: String,
    pub fields: BTreeMap<String, String>,
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl Failure {
    fn message(message: impl ToString) -> Self {
        Self {
            message: message.to_string(),
            fields: BTreeMap::new(),
        }
    }
    fn response(status: u16, status_text: &str, body: &str) -> Self {
        let value = serde_json::from_str::<Value>(body).unwrap_or(Value::Null);
        let message = value["error"]["message"]
            .as_str()
            .or(value["error"].as_str())
            .or(value["message"].as_str())
            .unwrap_or(status_text);
        let fields = value["error"]["fields"]
            .as_object()
            .map(|fields| {
                fields
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            value
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| value.to_string()),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            message: format!("HTTP {status}: {message}"),
            fields,
        }
    }
}
async fn send(
    method: &str,
    path: &str,
    body: Option<&Value>,
    controller: Option<&str>,
) -> Result<(String, Option<String>), Failure> {
    let window =
        web_sys::window().ok_or_else(|| Failure::message("Browser window is unavailable."))?;
    let abort = AbortController::new().map_err(|e| Failure::message(error(e)))?;
    let options = RequestInit::new();
    options.set_method(method);
    options.set_signal(Some(&abort.signal()));
    if let Some(body) = body {
        options.set_body(&JsValue::from_str(&body.to_string()));
    }
    let request =
        Request::new_with_str_and_init(path, &options).map_err(|e| Failure::message(error(e)))?;
    if body.is_some() {
        request
            .headers()
            .set("Content-Type", "application/json")
            .map_err(|e| Failure::message(error(e)))?;
    }
    if let Some(controller) = controller {
        request
            .headers()
            .set("X-Firecrab-Controller-Id", controller)
            .map_err(|e| Failure::message(error(e)))?;
    }
    let timeout_abort = abort.clone();
    let timeout = Closure::<dyn FnMut()>::new(move || timeout_abort.abort());
    let timer = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            timeout.as_ref().unchecked_ref(),
            15000,
        )
        .map_err(|e| Failure::message(error(e)))?;
    let result = async {
        let response: Response = JsFuture::from(window.fetch_with_request(&request))
            .await
            .map_err(|e| Failure::message(error(e)))?
            .dyn_into()
            .map_err(|e| Failure::message(error(e)))?;
        let text = JsFuture::from(response.text().map_err(|e| Failure::message(error(e)))?)
            .await
            .map_err(|e| Failure::message(error(e)))?
            .as_string()
            .unwrap_or_default();
        if !response.ok() {
            return Err(Failure::response(
                response.status(),
                &response.status_text(),
                &text,
            ));
        }
        let filename = response
            .headers()
            .get("Content-Disposition")
            .ok()
            .flatten()
            .and_then(|header| {
                header
                    .split("filename=\"")
                    .nth(1)
                    .and_then(|tail| tail.split('"').next())
                    .map(str::to_owned)
            });
        Ok((text, filename))
    }
    .await;
    window.clear_timeout_with_handle(timer);
    result
}
pub async fn request_detailed(
    method: &str,
    path: &str,
    body: Option<&Value>,
    controller: Option<&str>,
) -> Result<Value, Failure> {
    let (text, _) = send(method, path, body, controller).await?;
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text)
        .map_err(|error| Failure::message(format!("Invalid API response: {error}")))
}
pub async fn request(method: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
    request_with_controller(method, path, body, None).await
}
pub async fn request_with_controller(
    method: &str,
    path: &str,
    body: Option<&Value>,
    controller: Option<&str>,
) -> Result<Value, String> {
    request_detailed(method, path, body, controller)
        .await
        .map_err(|error| error.to_string())
}
pub async fn text_response(path: &str) -> Result<(String, Option<String>), String> {
    send("GET", path, None, None)
        .await
        .map_err(|error| error.to_string())
}
pub fn error(value: impl Into<JsValue>) -> String {
    let value = value.into();
    value.as_string().unwrap_or_else(|| format!("{value:?}"))
}
pub fn encode(value: &str) -> String {
    use std::fmt::Write;
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}
