use super::*;
use axum::{
    extract::{
        FromRequestParts, Request,
        ws::{Message, WebSocketUpgrade},
    },
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::SinkExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) async fn upgrade(
    state: AppState,
    s: Arc<Session>,
    request: Request,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Response {
    let (mut parts, _) = request.into_parts();
    let Ok(upgrade) = WebSocketUpgrade::from_request_parts(&mut parts, &()).await else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    upgrade.max_message_size(32*1024).on_upgrade(move|mut socket|async move{
        let _permit=permit;
        if !current(&state,&s){return;}
        let payload=STANDARD.encode(serde_json::to_vec(&serde_json::json!({"workspace":s.record.runtime.workspace_id,"commit":s.record.runtime.initial_commit})).unwrap());
        let code=STANDARD.encode(format!("{}\n{}\nimport base64\nterminal(json.loads(base64.b64decode('{}')))\n",include_str!("guest.py"),include_str!("terminal.py"),payload));
        let remote=format!("python3 -u -c 'import base64;exec(base64.b64decode(\"{code}\"))'");
        let dir=crate::code_runtime::directory(&state,s.record.runtime.workspace_id);
        let Ok(mut command)=guest::code_transport(&state,&s.binding,&dir,Some(&remote))else{return;};
        command.stderr(std::process::Stdio::null());let Ok(mut child)=command.spawn()else{return;};
        let (Some(mut input),Some(mut output))=(child.stdin.take(),child.stdout.take())else{return;};
        let mut stop=s.cancel.subscribe();let mut buffer=[0u8;8192];
        let work=async{loop{tokio::select!{
            result=output.read(&mut buffer)=>{let Ok(n)=result else{break;};if n==0||socket.send(Message::Binary(buffer[..n].to_vec().into())).await.is_err(){break;}},
            frame=socket.recv()=>{
                let value=match frame{
                    Some(Ok(Message::Binary(bytes))) if bytes.len()<=16384=>serde_json::json!({"input":STANDARD.encode(bytes)}),
                    Some(Ok(Message::Text(text))) if text.len()<=128=>{
                        #[derive(Deserialize)]#[serde(deny_unknown_fields)]struct Size{rows:u16,cols:u16}
                        let Ok(size)=serde_json::from_str::<Size>(&text)else{break;};if !(2..=1000).contains(&size.rows)||!(2..=1000).contains(&size.cols){break;}
                        serde_json::json!({"rows":size.rows,"cols":size.cols})
                    },
                    Some(Ok(Message::Ping(_)|Message::Pong(_)))=>continue,
                    _=>break,
                };
                let frame=serde_json::to_vec(&value).unwrap();if input.write_all(&(frame.len() as u32).to_be_bytes()).await.is_err()||input.write_all(&frame).await.is_err(){break;}
            }
        }}};tokio::select!{_=work=>{},_=stop.changed()=>{}}
        let _=child.kill().await;let _=tokio::time::timeout(Duration::from_millis(100),socket.close()).await;
    })
}
