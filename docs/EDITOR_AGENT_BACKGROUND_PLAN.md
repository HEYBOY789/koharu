# Editor Agent: tiến trình rõ ràng và chạy nền theo project

Ngày: 31/10/2026. Trạng thái: plan triển khai, chưa thay đổi hành vi ứng dụng.
Baseline đã kiểm tra: `9b6d5a80`.

## Kết quả cần có

Agent đang chỉnh layer của project A vẫn tiếp tục khi người dùng đóng editor A, mở project B và cho B dịch. Activity hiển thị đúng tên project, thao tác hiện tại và Stop. Mở lại A thấy trạng thái hội thoại cùng các chỉnh sửa đã commit. Đóng ứng dụng vẫn dừng tác vụ; phạm vi này không có tự phục hồi sau khi tắt máy.

Làm tiến trình trước, chạy nền sau. Giữ giới hạn một yêu cầu Editor Agent đang chạy; người dùng vẫn được chạy workflow dịch của project khác. Khi agent A đang chạy, panel B chỉ rõ tác vụ A đang bận và cho mở A/Stop, chưa nhận yêu cầu agent thứ hai.

## Những ràng buộc từ source

- `crates/koharu-app/src/commands/lifecycle.rs`: `prepare_project_switch` gọi `AgentState.reset`, hủy lượt và xóa phiên khi mở/đóng project.
- `crates/koharu-app/src/commands/agent.rs`: tác vụ dùng channel của panel; lỗi gửi sự kiện sẽ hủy control. Bỏ riêng lệnh reset khi chuyển project chưa đủ để chạy nền.
- `crates/koharu-app/src/commands/project/workspace.rs`: workspace có một active project, một background project và một pipeline target. Muốn giữ agent A cùng pipeline B khi mở C phải thay cách giữ project, không thêm một slot A riêng cạnh slot pipeline.
- `crates/koharu-app/src/commands/agent/host.rs`: tool đọc/sửa project đang active và luôn đồng bộ canvas. Project binding hiện từ chối đổi instance. Các phần này phải chuyển sang truy cập project đích đã giữ cho run.
- `Processing.wait` đã có admission dùng chung và hủy khi đang chờ. Agent gọi pipeline hiện dùng `try_admit`, nên đang bận sẽ lỗi ngay.
- `AgentPanel.tsx` chỉ giữ transcript/trạng thái trong component. AGY parser mới đọc text/result; UI chỉ hiển thị nhãn chung, chưa trình bày reasoning được backend công bố.

## Thiết kế sẽ triển khai

### 1. Tiến trình có nguồn dữ liệu thật

Native session là nguồn duy nhất của transcript, run state và tiến trình; panel và Activity cùng subscribe. Sự kiện có run/project identity và thứ tự để dựng lại snapshot rồi nhận tiếp mà không mất/nhân đôi message. Rời panel chỉ unsubscribe; Stop mới hủy lượt.

Tiến trình lấy từ tool/stage thật: đọc layer trang nào, xem ảnh trang nào, sửa typography cho element nào, thao tác đã thành công/thất bại hoặc đang chờ pipeline. Chỉ tăng số thao tác sau kết quả thành công. Không coi đọc một batch là kiểm tra xong toàn project và không dựng phần trăm hoàn tất bằng phỏng đoán.

Giữ một vùng trạng thái có chiều cao ổn định; gộp cập nhật liên tiếp, tránh chuyển nhãn Working/Applying quá nhanh. Một placeholder khi chưa có câu trả lời, không lặp hai dòng Working. Giữ vị trí cuộn nếu người dùng đang đọc phía trên.

Adapter chỉ chuyển reasoning/summary hoặc tín hiệu thinking mà CLI công bố trong giao thức đã xác minh. Nội dung được công bố có thể mở trong khung thu gọn; backend không cung cấp thì hiển thị tiến trình tool. Không lấy nội dung từ log nội bộ để giả làm stream hỗ trợ chính thức. Giới hạn bộ nhớ transcript/timeline và gộp delta dày để tránh tải lại lỗi lag.

### 2. Project ownership và vòng đời agent

Mở rộng module `ProjectWorkspace` để giữ các session project theo nhu cầu của editor, pipeline và agent. Một project có một session/Undo history dùng chung; mỗi tác vụ giữ một quyền truy cập gắn instance, tự nhả khi kết thúc. Project không còn editor hay tác vụ giữ thì được giải phóng. Mở lại phải lấy đúng session đang được giữ, không mở thêm bản sao ghi vào cùng file.

Native Editor Agent session giữ project đích và cấu hình backend/model của run. Tool host lấy snapshot/commit qua quyền đó, không tra lại active project. Cancellation và kiểm tra instance vẫn thực hiện trước đọc/sửa; lời gọi đến muộn sau khi kết thúc hoặc bị hủy phải bị từ chối. Cấu hình của lượt đang chạy không đổi khi điều hướng.

Hội thoại/trạng thái để panel dựng lại thuộc native và được phân theo project; không trộn output A vào B. Trong thời gian run, giữ nguyên CLI conversation và tool bridge. Giới hạn phiên/history rảnh và đóng CLI khi session owner giải phóng để tránh tích tụ tiến trình. Snapshot chat/tiến trình không chứa toàn bộ context project hoặc ảnh base64. Giữ cơ chế context tối đa 64 KiB và các tool đọc phân trang đã có.

Thay đổi nền vẫn commit và ghi Undo vào đúng A. Chỉ đồng bộ renderer/canvas và refresh query của project đang hiển thị. Render ảnh xem trước cho agent từ snapshot A qua đường render hiện có; không chuyển canvas B sang A để chụp ảnh.

### 3. Pipeline, Stop và đóng ứng dụng

Agent sửa layer A có thể chạy trong lúc pipeline B dịch. Khi agent gọi OCR/dịch/inpainting, chờ bằng admission hiện có; không tạo lane suy luận hay hàng đợi thứ hai. Sau khi lấy được permit, đọc snapshot mới của project đích. Không giữ khóa workspace trong lúc chờ permit hoặc CLI. Stop phải hủy được cả trạng thái đang chờ.

Giữ một writer xử lý nặng trên cùng project: yêu cầu khởi chạy pipeline bên ngoài vào project đang được agent giữ để sửa phải bị chặn với trạng thái rõ ràng; pipeline do chính agent yêu cầu dùng cùng run ownership. Tool sửa phải kiểm tra revision/precondition của dữ liệu đã đọc; cập nhật schema/consumer ở seam này nếu còn thiếu precondition. Khi người dùng đã đổi dữ liệu liên quan, từ chối thay đổi cũ và cho agent đọc lại, không ghi đè một cách im lặng.

Đóng editor hoặc chuyển project chỉ đổi lựa chọn hiển thị. Xóa project có tác vụ đang giữ phải yêu cầu dừng tác vụ đó trước. Khi thoát ứng dụng, hủy run, thu hồi tool lease, chờ thao tác đang commit hoàn tất và dừng đúng cây tiến trình CLI; giữ các commit đã hoàn thành.

## Thứ tự và điều kiện xong

1. **Xác minh sự kiện và tiến trình.** Ghi fixture sự kiện public thực tế của CLI; nối tool/page/stage và phần summary được hỗ trợ vào một mô hình tiến trình. Xong khi trạng thái hiển thị tương ứng với sự kiện thật, có regression cho delta dày và terminal/error; chưa đổi hành vi đóng project ở bước này.
2. **Giữ và truy cập project theo tác vụ.** Thay interface workspace và cập nhật toàn bộ consumer pipeline/editor/agent; không thêm alias tương thích. Xong khi A-agent, B-pipeline, C-editor truy cập độc lập, mở lại dùng cùng instance và kết thúc một tác vụ không đóng project tác vụ khác còn giữ.
3. **Tách run khỏi panel.** Native lưu snapshot/transcript, subscribe độc lập; đổi project không reset run; tool host và canvas dùng đúng target. Xong khi unmount/mất channel/reconnect không hủy run, output A không lọt B, Stop vẫn hủy và không còn tool muộn.
4. **Ghép Activity và admission.** Tận dụng Activity hiện có, cùng nguồn run state/cancel path; không bọc agent thành pipeline job giả hoặc nhân đôi bộ đếm. Xong khi B dịch trong lúc A sửa, agent chờ pipeline có thể Stop, tác vụ không deadlock hoặc giữ khóa editor khi đợi.
5. **Xác minh và bàn giao.** Generate bridge từ Rust bằng generator chính thức; kiểm tra TypeScript, locale, focused tests, frontend export và Windows debug custom-protocol build. Cập nhật tài liệu setup/workflow đã thay đổi, commit/push source lên nhánh bàn giao và cài exe có backup/hash. Không thay executable đang chạy bằng cách ép đóng phiên người dùng.

## Ca nghiệm thu bắt buộc

- [ ] Tool/page/stage hiện rõ; trạng thái không chớp, không đẩy cuộn khi đang đọc lịch sử; label dài không tràn.
- [ ] A đang sửa, chuyển sang B cho dịch, rồi mở C: A/B tiếp tục và C không bị đổi canvas/layer.
- [ ] Mở lại A giữa run và sau khi xong: panel nhận snapshot/sự kiện đúng thứ tự, không mất/nhân đôi output; layer và Undo cùng session.
- [ ] Stop A khi đang gọi tool và khi đang chờ admission: B tiếp tục, commit A đã xong còn giữ, lời gọi muộn bị từ chối.
- [ ] Xong A nhưng B còn chạy không giải phóng nhầm project B; xóa project đang được giữ bị chặn đúng đích.
- [ ] Cạnh tranh chỉnh sửa cùng project được xử lý theo revision/precondition, không áp kết quả từ snapshot cũ lên dữ liệu đã khác.
- [ ] Thoát app dọn đúng CLI/process group và quyền tool; không tuyên bố tiếp tục sau khi khởi động lại.
- [ ] 11 locale đầy đủ; typed bridge sinh đúng từ nguồn; regression hiện có về Undo, context lớn, pipeline nền và hủy vẫn qua.
- [ ] Có ảnh trạng thái UI thực tế và bằng chứng phiên native dùng được. Nếu capture hoặc backend chưa dùng được trong môi trường kiểm tra, ghi rõ phần chưa xác minh; build xanh không thay cho nghiệm thu đó.

Đoạn này là tiêu chí cần thực hiện, không phải báo cáo đã chạy. Dùng focused tests/kiểm tra native theo quy tắc repo; chạy bộ E2E tự động chỉ khi người dùng yêu cầu.
