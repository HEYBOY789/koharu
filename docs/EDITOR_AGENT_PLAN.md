# Koharu Editor Agent: Pi và AGY

Ngày: 31/10/2026. Trạng thái: đã triển khai source; đã qua kiểm tra Windows, chưa xác minh chạy thực tế trên Linux/native UI.

## Mục tiêu

Trong tab Agent của editor, chọn Codex, Pi hoặc AGY để xem trang và chỉnh layer trong project đang mở. Các backend dùng chung thao tác project, renderer và Undo hiện có. Giữ nguyên Codex hiện tại; Pi/AGY dùng harness và đăng nhập riêng của chúng.

Phạm vi này dành cho Agent editor. Luồng tải truyện, tóm tắt/wiki và chuẩn bị FAQ/giới thiệu đang dùng CLI là các tác vụ khác, không thay đổi trong đợt này.

## Bằng chứng hiện có

- `crates\koharu-agent\src\agent.rs`: vòng lặp agent, lịch sử, streaming và điều khiển hủy hiện gắn trực tiếp với Codex.
- `crates\koharu-app\src\commands\agent\host.rs`: `KoharuHost` đã sở hữu context project, ảnh trang và tool chỉnh sửa; thay đổi đi qua thao tác project và lịch sử Undo.
- `crates\koharu-app\src\commands\agent.rs`: quản lý một yêu cầu editor đang chạy, login và reset phiên.
- `packages\koharu\components\editor\AgentPanel.tsx`: giao diện hiện phụ thuộc vào tài khoản/model Codex.
- AGY cài trên máy: `--input-format stream-json`, `--output-format stream-json`, cấu hình theo phiên qua `--settings`, hỗ trợ MCP stdio/http. Đã kiểm tra bằng AGY thật qua adapter native: hai lượt giữ cùng hội thoại, tool sửa/xem ảnh hoạt động. Profile không khai báo built-in tool components (`tools: []`); MCP do CLI nạp từ plugin riêng.
- Pi có [RPC cho ứng dụng nhúng](https://pi.dev/docs/latest/rpc), gồm tiến trình chạy liên tục, lệnh/sự kiện JSONL và hủy/đóng phiên. Tài liệu lưu ý `--help` của bản cài là nguồn quyết định. Người dùng xác nhận chưa cài Pi. Đã thử Pi 1.4.0 trong thư mục build riêng bằng model giả lập cục bộ, xác minh tool text/ảnh/sửa và sự kiện hoàn tất `agent_settled`. Runtime hỗ trợ Pi 1.4+ trong nhánh 1.x; chưa xác minh provider thật đã đăng nhập của Pi.

## Thiết kế

Seam đặt giữa phiên Agent editor và từng harness. Mỗi adapter chịu trách nhiệm giao tiếp, lịch sử hội thoại, model và vòng gọi tool của backend đó. Giao diện gọi một module phiên editor nhỏ để kiểm tra khả năng chạy, cấu hình, gửi yêu cầu và hủy.

- Codex adapter tiếp tục dùng vòng lặp Rust và đăng nhập hiện có.
- AGY adapter chạy CLI liên tục qua stream-json, gọi tool Koharu qua MCP được cấu hình riêng cho phiên.
- Pi adapter chạy RPC; đăng ký tool qua interface được bản Pi thực tế hỗ trợ. Kiểm chứng cách nhận/trả ảnh trước khi chốt extension hoặc MCP.
- Bộ tool vẫn do `KoharuHost` sở hữu. Adapter chỉ chuyển đổi giao thức và schema, không viết lại thao tác sửa layer hoặc ghi trực tiếp file project.
- Native sở hữu tiến trình và cầu nối tool. Frontend tiếp tục dùng typed commands/events; không thêm HTTP client hay giao thức sự kiện tùy ý vào UI.
- Dùng chung `RunId`, cơ chế hủy và cập nhật renderer/Undo hiện có. Không tạo job queue editor thứ hai.

Phiên gắn với định danh project và lần mở project. Khi đóng/chuyển project, vô hiệu hóa quyền gọi tool của phiên cũ, hủy tác vụ và từ chối lời gọi tới muộn. Không dựa riêng vào việc UI đã xóa hội thoại. Thao tác sửa vẫn tuân theo revision/precondition của scene; không tự áp lại thay đổi lên dữ liệu đã khác.

Khi đổi backend lúc rảnh, bắt đầu hội thoại mới; khóa đổi backend khi yêu cầu đang chạy. Không chuyển lịch sử thô giữa các harness. Trạng thái sẵn sàng của Pi/AGY dựa trên CLI của chúng, không yêu cầu đăng nhập Codex trước.

## Các bước triển khai

### 1. Kiểm chứng giao thức

- [x] Xác định binary/phiên bản Pi và AGY, cách chọn model và kiểm tra đăng nhập không làm thay đổi tài khoản toàn máy.
- [x] Kiểm chứng cấu hình tool theo phiên, tránh ghi đè MCP/settings toàn cục của người dùng.
- [x] AGY thật và Pi 1.4 với provider giả lập đã chứng minh tool text/ảnh/sửa. Kiểm tra module CLI chung phủ lỗi, hủy và thoát tiến trình bằng harness thử riêng; không dùng project của người dùng.
- [ ] Xác minh Pi với provider thật đã đăng nhập; người dùng xác nhận chưa cài Pi.
- [x] Ghi lại interface/schema đã xác minh và các giới hạn phiên bản. Không hiển thị backend là sẵn sàng nếu thiếu khả năng cần thiết.

### 2. Module phiên editor và tool dùng chung

- [x] Tách phần Codex khỏi quản lý phiên editor, giữ cùng hành vi hiện tại và cùng tool host.
- [x] Ràng buộc project/phiên cho mọi lời gọi tool; kiểm tra lại trước khi thực hiện thao tác có thay đổi.
- [x] Chuyển schema, ảnh và lỗi qua cầu nối; giữ thao tác semantic hiện có, không xuất/import lại project để chỉnh layer.
- [x] Bảo đảm Stop ngăn lời gọi mới, hủy pipeline đang chạy theo cơ chế hiện có và thu hồi kết nối tool. Giữ các chỉnh sửa đã commit và có Undo.

### 3. AGY adapter

- [x] Quản lý tiến trình, stream-json, hội thoại, model và tool MCP theo kết quả bước 1.
- [x] Map câu trả lời/sự kiện tool/lỗi/hủy vào typed events hiện có; log lỗi CLI không chứa thông tin xác thực.
- [x] Xử lý tiến trình thoát, output sai định dạng và gọi tool tới muộn mà không treo editor.

### 4. Pi adapter

- [x] Quản lý RPC và đăng ký tool theo phiên bản đã xác minh.
- [x] Nhận streaming, tool result/ảnh và trạng thái hoàn tất đúng giao thức; đọc stdout liên tục để không nghẽn tiến trình.
- [x] Áp dụng cùng quy tắc project, Stop, lỗi và đóng phiên như AGY.

### 5. Giao diện và cấu hình

- [x] Thêm lựa chọn Codex / Pi / AGY ngay trong Agent panel hiện có.
- [x] Cấu hình theo backend: model, reasoning nếu hỗ trợ; CLI/Node nếu cần nằm trong phần nâng cao có tự dò.
- [x] Hiện lỗi thiếu binary/chưa đăng nhập/không tương thích ngay tại panel; lỗi lấy model không tạo trạng thái loading vô hạn.
- [x] Giữ đăng nhập Codex trong lựa chọn Codex; Pi/AGY dùng trạng thái đăng nhập của CLI tương ứng.
- [x] Cập nhật cấu hình authoritative, typed commands và mọi consumer; chạy generator bridge chính thức khi contract thay đổi. Không thêm alias/compatibility shim.
- [x] Toàn bộ nhãn qua i18n đủ 11 locale, dùng control library hiện có; không thêm panel chat thứ hai.

### 6. Xác minh và bàn giao

- [x] Focused checks ở interface phiên: khởi chạy, stream bị chia mảnh, kết thúc/lỗi, hủy, crash, project đổi trong lúc tool đang chờ và phiên đã bị vô hiệu hóa.
- [x] Focused host checks: một sửa text/typography qua adapter có cùng kết quả scene, renderer và Undo như sửa trong editor; không sửa nhầm project.
- [x] Focused UI checks: chọn backend, trạng thái đăng nhập độc lập Codex, lỗi công cụ và khóa đổi backend khi chạy.
- [x] Chạy check/build debug phù hợp phần thay đổi, không chạy toàn bộ suite hoặc suite E2E.
- [x] Kiểm tra thực tế trên project thử với từng backend: xem ảnh, sửa một layer, kiểm tra Undo và Stop. Ghi rõ backend nào chưa được kiểm chứng thực tế.
- [ ] Chụp UI chọn backend và trạng thái chạy/lỗi; nếu công cụ capture không hoạt động, ghi rõ giới hạn, không coi build xanh là xác nhận giao diện.
- [x] Cập nhật tài liệu và kiểm tra build. Theo yêu cầu mới, bàn giao source qua Git; không đưa executable lên repo hoặc đóng phiên Windows đang mở để cài bản mới.

## Điều kiện hoàn tất

Người dùng chọn Pi hoặc AGY trong tab Agent, gửi yêu cầu xem/sửa trang, thấy kết quả cập nhật trong editor và Undo được. Stop, lỗi CLI và đổi project không để agent tiếp tục sửa project khác. Codex và các workflow dịch/wiki đang có vẫn hoạt động. Backend chưa đạt các điều kiện trên phải được ghi là chưa hoàn tất, không dùng kết quả JSON giả lập để tuyên bố đã tích hợp.

## Kết quả kiểm tra — 31/10/2026

- 3 kiểm tra module CLI: stream, tool/ảnh, cấp và thu hồi quyền giữa các lượt, hủy và record JSON bị chia nhỏ.
- 1 kiểm tra host trên project thử: sửa text, Undo, hủy quyền và từ chối project mở lại.
- 3 kiểm tra UI: không cần tài khoản Codex cho AGY, khóa cấu hình khi đang chạy, lưu Pi setup và hồi phục từ lỗi kiểm tra kết nối.
- 1 kiểm tra AGY thật (chạy riêng, không tự gọi model trong suite mặc định): hai lượt cùng conversation, gọi tool sửa/xem ảnh và giữ ngữ cảnh.
- Pi 1.4.0: chạy CLI/extension thật với provider giả lập cục bộ để kiểm tra giao thức; không thay tài khoản Pi hoặc dùng project của người dùng.
- TypeScript, 11 locale và frontend export đã qua. Bản Windows debug đã được dùng để kiểm tra build local; bản bàn giao là source. Không xác nhận đã chạy ứng dụng trên Linux hoặc đã chụp giao diện native mới.

## Source và đa nền tảng

Nhánh bàn giao dự kiến: `codex/editor-agent-harnesses-20261031` trên fork Koharu của người dùng. Source, generated bridge và lockfile được commit; executable, model, cache, credential và dữ liệu truyện không được đưa vào Git.

Windows dùng executable/Node được dò trên máy và dừng cây tiến trình. Unix dùng executable/Node trong PATH hoặc `~/.local/bin` và dừng process group riêng. Mỗi máy cần build Koharu, cài CLI/Node và đăng nhập tương ứng. Các công cụ PSEO/downloader của workflow khác vẫn cần publishing toolkit riêng theo cấu hình; Editor Agent không phụ thuộc đường dẫn toolkit.

Lệnh kiểm tra tập trung:

```sh
cargo test -p koharu-agent cli::tests --lib
cargo test -p koharu-app --lib agent_binding_preserves_undo_and_rejects_cancelled_or_reopened_projects
```

Kiểm tra AGY thật được đánh dấu ignored và chỉ chạy chủ động với `KOHARU_EDITOR_PROBE_BACKEND=agy` cùng binary/model tùy chọn. Chế độ `--conversation` kết hợp stream-json đã tạo hội thoại mới trong thử nghiệm; vì vậy implementation giữ CLI liên tục thay vì tạo lại tiến trình mỗi lượt. Quyền tool được cấp lại theo từng lượt và bị thu hồi khi lượt kết thúc.
