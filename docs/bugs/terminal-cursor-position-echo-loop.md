# 터미널 프롬프트에 `;1R;80R;1R;80R...` 반복 출력

`terminal` 버튼으로 접속한 xterm.js 세션에서 프롬프트에 이런 garbage가 반복 출력되는 버그
(2026-07-21 발견·수정).

## 원인

xterm.js는 guest의 커서 위치 질의(`ESC[6n`, DSR)를 받으면 자동으로 `ESC[row;colR` 응답을
`onData` 이벤트로 만들어내고, `Console.tsx`는 그걸 그대로 WebSocket으로 guest에 릴레이한다.
문제는 guest의 tty가 받은 바이트를 그대로 output으로 echo한다는 것 — 그 echo를 xterm.js가 다시
파싱하면서 몇 라운드 주고받다가 화면에는 `ESC[` 부분이 안 보이는 `;1R`·`;80R` 조각만 반복 출력된
것처럼 보인다. `ConsoleBroker`의 backlog가 이 내용을 저장하므로, 한 번 발생하면 이후 새로 접속하는
모든 클라이언트도 backlog 재생 시 같은 garbage를 보게 된다.

## 수정

`Console.tsx`의 `term.onData` 핸들러에서 `ESC[숫자;숫자R` 모양(`/^\x1b\[\d+;\d+R$/`)인 데이터는
서버로 보내지 않고 버린다. 실제 사용자 키 입력은 이 정확한 모양을 절대 만들지 않으므로 정상 입력에는
영향이 없다.

## 검증

신규 VM 생성·시작 → 헤드리스 브라우저로 터미널 버튼 클릭 → 실제 부팅 로그 뒤 프롬프트
(`root@firecrab:~#`)까지 깨끗하게 렌더링되고 garbage 없음을 스크린샷·DOM 텍스트 검사로 확인.

## 산출물

`firecrab-frontend/src/components/Console.tsx`
