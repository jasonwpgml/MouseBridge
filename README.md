# MouseBridge

맥 한 대와 Windows PC 여러 대를 마우스·키보드 하나로 같이 쓰는 프로그램이에요.
[Lan Mouse](https://github.com/feschber/lan-mouse)를 바탕으로, 맥과 Windows를 오갈 때
불편한 점을 고쳐 나가고 있어요.

Share one mouse and keyboard between a Mac and Windows PCs. Based on Lan Mouse.

## Lan Mouse와 다른 점

- **Cmd↔Ctrl, 한/영 키**: 기기마다 키를 바꿔 보낼 수 있어요. 맥의 Cmd+C/V가 Windows에서
  Ctrl+C/V로, 오른쪽 Cmd가 한/영 전환으로 동작해요. Windows에서는 한/영·한자 키를 가상 키로
  보내서 키보드 종류와 상관없이 IME가 바뀌어요.
- **두 번째 커서 (Windows, 실험 중)**: 맥에서 넘어온 마우스가 자기 커서를 따로 가져요. 기기
  이름이 붙은 색깔 화살표로 보이고, 데스크톱 마우스는 원래 커서를 그대로 써요.
- **모니터 크기가 다른 Windows**: 키가 다른 모니터를 나란히 둬도 각 모니터의 빈 가장자리로
  넘어갈 수 있어요.
- **연결 끊김 수정 (Windows)**: 한 번 연결이 끊기면 다시 받지 못하던 문제를 고쳤어요
  ([lan-mouse#262](https://github.com/feschber/lan-mouse/issues/262)).
- **화면 배치**: 설정 창의 Layout에서 기기마다 실제 모니터 크기대로 끌어다 놓아요. 화면이
  맞닿은 구간으로만 넘어가고, 넘어간 자리 그대로 들어가요. 가장자리 하나를 여러 기기가 나눠 쓸
  수 있어요. 모서리를 끌면 크기가 바뀌고, 더블클릭하면 실제 크기로 돌아와요 (맥·Windows,
  이 기기에서 나가는 쪽에 적용).
- **키보드 모드**: 키보드가 커서를 따라갈지, 마지막으로 클릭한 기기로 갈지, 이 기기에만 남을지,
  한 기기로 고정될지 골라요. `click`이면 어느 마우스로 클릭했든 마지막으로 클릭한 기기의 창에
  입력돼요. 실행 중에는 `mousebridge cli keyboard follow|click|local|<기기 이름>`으로 바꿔요
  (맥·Windows).
- **설치 스크립트**: 맥에서 한 번 실행하면 Windows PC에 붙여넣을 명령까지 만들어 줘요.
  로그인할 때 창 없이 자동으로 켜져요.

## 설치

맥에서:

```sh
bash <(curl -fsSL https://raw.githubusercontent.com/jasonwpgml/MouseBridge/main/scripts/setup-mac.sh) DESKTOP-ABC:top LAPTOP-XYZ:right
```

`이름:위치`는 Windows PC 이름과, 맥 화면 기준 그 PC가 있는 쪽(left/right/top/bottom)이에요.
스크립트가 PC마다 PowerShell 명령을 클립보드에 넣어 주니, 그 PC에서 붙여넣으면 돼요.
Lan Mouse를 쓰고 있었다면 설정과 인증서를 그대로 가져와요.

설정 파일은 맥은 `~/.config/mousebridge/config.toml`, Windows는
`%LOCALAPPDATA%\mousebridge\config.toml`에 있어요.

## 설정

```toml
# 맥에서 넘어온 마우스를 두 번째 커서로 보여 줘요 (Windows)
second_cursor = true
# 이 기기 키보드가 갈 곳: "follow"(커서 따라감, 기본), "click"(마지막으로 클릭한 기기),
# "local"(이 기기에만), 또는 기기 이름
keyboard = "follow"
# 배치판에서 이 기기의 자리와 크기(%). 설정 창 Layout에서 끌면 저장돼요
placement = { x = 0, y = 1080, size = 100 }

[[clients]]
hostname = "DESKTOP-ABC"
position = "top"
activate_on_startup = true
# 배치하면 position 대신 맞닿은 구간으로 넘어가요
placement = { x = 0, y = 0, size = 100 }

# 이 기기로 보내는 키 바꾸기
[key_remap."DESKTOP-ABC"]
KeyLeftMeta = "KeyLeftCtrl"
KeyLeftCtrl = "KeyLeftMeta"
KeyF18 = "KeyHanguel"
```

나머지 설정과 빌드 방법은 [Lan Mouse 문서](docs/LAN_MOUSE_README.md)와 같아요.

## 만든 사람과 라이선스

Made by JSonJh (https://github.com/jasonwpgml)

[Lan Mouse](https://github.com/feschber/lan-mouse) © Ferdinand Schober와 기여자들의 코드를
바탕으로 해요. 원본과 같은 [GPL-3.0-or-later](LICENSE) 라이선스를 따라요.

[//]: # (fd2b31e3ef52)
