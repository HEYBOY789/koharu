# Rà soát Editor Agent với thiết kế hiện tại

Ngày: 31/10/2026. Source đã kiểm tra: `b4ce18dcbedc0f805145d8d2940e0189abd2349d`.

Phạm vi: toàn bộ 16 tool của `KoharuHost`, context đầu vào, cách native thực thi thay đổi, chapter workflow, phạm vi pipeline, phiên chạy nền và thông tin lựa chọn từ editor. Đây là rà soát source; không chạy model, pipeline hoặc sửa project người dùng. Chưa thay đổi runtime, CLI, executable hay schema trong lần rà soát này.

## Kết luận

Các tool đã dùng project session được giữ riêng cho tác vụ, native commits và Undo. Chúng chưa cung cấp interface đầy đủ cho project nhiều chapter. Không cần viết lại harness cho từng backend: Codex, Pi và AGY đều dùng cùng `KoharuHost`, nên cần sửa tại context/tool interface chung.

Phân biệt ba loại kết quả dưới đây: lỗi/nguy cơ từ cách nhận tham số, thiếu khả năng cần cho workflow mới, và khả năng editor chưa được cung cấp cho agent. Không coi mọi tính năng còn thiếu là lỗi hồi quy.

## Các phát hiện

### 1. Ưu tiên cao: phạm vi pipeline mặc định mở rộng thành cả project

`RunPipeline` chỉ nhận `operation`, `pages`, `elements`. Hai danh sách mặc định rỗng; cả hai rỗng trở thành `Scope::Project`. Không có trường chapter hoặc khoảng trang. Native deserialization không từ chối trường không biết. Vì vậy, nếu nhận `{ "operation": "translation", "chapter": "..." }`, trường chapter không giới hạn tác vụ và native sẽ chọn toàn project. Đây là nguy cơ từ interface, không phải khẳng định agent đã gửi payload đó trong phiên của người dùng.

Ngoài ra, mọi lần agent gọi pipeline đều đặt `resume: false`. Agent chưa dùng được cơ chế tiếp tục và bỏ qua kết quả hoàn thành của workflow mới. Có thể chạy lại kết quả máy đã tạo; translation stage vẫn bảo vệ bản dịch có `Origin::User`, nên không kết luận rằng mọi chỉnh sửa tay đều bị ghi đè.

Bằng chứng: `crates/koharu-app/src/commands/agent/host.rs:177`, `:214`, `:901`; `crates/koharu-pipeline/src/stages/translation.rs:199`.

Hướng sửa: phạm vi phải được chọn tường minh; trường không hỗ trợ phải bị từ chối. Nối chế độ resume và bộ giải quyết chapter/khoảng trang vào chủ sở hữu workflow hiện tại, không tạo một pipeline dịch riêng.

### 2. Ưu tiên cao: thiếu text có thể trở thành lệnh xóa bản dịch

`SetTranslation.text` là `Option<String>`. Trong native deserialization, thiếu trường này và gửi `null` đều thành `None`. `Project::set_translation(..., None)` xóa thành phần bản dịch. Tool description chỉ nói gửi null để xóa, nhưng hiện `{ "element": "<valid text-layer ID>" }` cũng mang cùng nghĩa. Lỗi sai tên trường có thể không báo như trường hợp thiếu `element` trong ảnh người dùng.

Bằng chứng: `crates/koharu-app/src/commands/agent/host.rs:294`, `:479`, `:767`; `crates/koharu-app/src/commands/project.rs:904`.

Hướng sửa: phân biệt bắt buộc có trường text với giá trị null có chủ đích; từ chối tham số thiếu/sai tên trước khi sửa project. Không tự đoán ID hoặc nội dung để vá một lời gọi tool không hợp lệ.

### 3. Chapter và khoảng trang chưa được mô tả đầy đủ

`inspect_project` trả danh sách trang theo thứ tự project, mỗi trang có ID, label, chapter ID và kích thước. `inspect_page` cũng chỉ trả chapter ID. Không trả tên/thứ tự chapter, số thứ tự trang trong chapter, số trang trong chapter hoặc danh sách chapter. `ProjectInfo` chỉ có tên project, revision, trang đang mở và trạng thái Undo/Redo.

Agent có thể nhóm các trang cùng chapter ID, nhưng không ánh xạ chắc chắn yêu cầu “trang thứ 3 của Chapter 1”. Chapter rỗng còn không thể được phát hiện qua danh sách trang. Hiện instruction yêu cầu đọc tiếp các batch trang, nhưng chưa quy định phân biệt nhãn trang, vị trí trong chapter và vị trí toàn project.

Bằng chứng: `crates/koharu-app/src/commands/agent/context.rs:44`, `:71`; `crates/koharu-app/src/commands/project.rs:31`; `crates/koharu-agent/src/agent.rs:15`.

Ngoài ra, run chỉ giữ project lease/revision; không giữ tập trang được phép sửa theo yêu cầu người dùng. Tool mutation kiểm tra đúng project và revision, nhưng chưa chặn việc sửa trang nằm trước điểm bắt đầu. Đây là khả năng cần thêm, không phải khẳng định bộ kiểm tra project hiện có bị bỏ qua.

Hướng sửa: đọc chapter có phân trang/budget; cung cấp vị trí trang theo chapter và project, đồng thời phân giải điểm bắt đầu/kết thúc thành các ID có thứ tự. Nếu cần bảo đảm tuyệt đối không sửa ngoài phạm vi, native phải giữ và kiểm tra phạm vi đó cho cả mutation lẫn pipeline; chỉ thêm câu nhắc trong prompt chưa tạo ra bảo đảm này.

### 4. Workflow chapter, summary, setting và wiki chưa được nối vào Editor Agent

UI/native đã có danh sách chapter, summary, setting, wiki và `run_chapters` với tùy chọn summarize/translate/resume. Editor Agent không có tool đọc hoặc chạy các chức năng này. `run_pipeline` đi thẳng vào page pipeline, không qua bước summarize-before-translate của chapter workflow.

Cần phân biệt: translation stage vẫn đọc setting đã lưu cho chapter của từng trang. Phần chưa nối là agent xem/chỉnh dữ liệu đó và tạo/cập nhật summary/wiki trước khi dịch; không phải pipeline hoàn toàn bỏ qua chapter setting.

Bằng chứng: `crates/koharu-app/src/commands/chapter.rs:25`, `:78`, `:313`, `:515`; `crates/koharu-app/src/commands/agent/host.rs:268`; `crates/koharu-pipeline/src/stages/translation.rs:58`.

Hướng sửa: mở khả năng đọc dữ liệu chapter/wiki có giới hạn; cho agent yêu cầu workflow hiện tại với target project được giữ và cùng cơ chế admission/cancellation. Không gọi nguyên command UI đang phụ thuộc project hiển thị khi agent thực sự giữ một project khác.

### 5. Đổi typography yêu cầu agent gửi lại toàn bộ định dạng

`set_typography` là replacement, không phải cập nhật một thuộc tính. Các trường Option bị bỏ khỏi lời gọi trở thành None và thay giá trị cũ. Ví dụ chỉ gửi font và auto_fit có thể xóa các thiết lập màu, viền, cỡ chữ hoặc căn chỉnh đã lưu. Description có nói Replace, nên đây là interface dễ dùng sai khi người dùng chỉ yêu cầu “sửa font”, không phải một phép merge đang hỏng.

UI Inspector tránh vấn đề này bằng cách lấy typography hiện có rồi cập nhật thuộc tính người dùng đổi; tool hiện để agent tự thực hiện việc đó.

Bằng chứng: `crates/koharu-app/src/commands/agent/host.rs:298`, `:495`, `:827`; `crates/koharu-app/src/commands/project.rs:931`; `packages/koharu/components/editor/Inspector.tsx:149`.

Hướng sửa: cập nhật có chủ đích các thuộc tính được yêu cầu và giữ phần còn lại; vẫn hỗ trợ xóa một thiết lập khi agent yêu cầu rõ ràng. Dùng cùng biểu diễn typography native, không thêm định dạng font/màu song song.

### 6. Tool di chuyển còn thiên về project phẳng và cây layer

`move_page` nhận index toàn project. Native có cập nhật membership chapter theo vị trí đích, vì vậy tool này thực tế có thể chuyển trang sang chapter khác. Description không nói tác dụng đó và kết quả không trả chapter mới/vị trí theo chapter.

`move_element` đổi parent/thứ tự trong cây layer. Nó không di chuyển hộp chữ theo tọa độ ảnh. Tên và progress “Moving layer” dễ bị hiểu nhầm trong yêu cầu sửa chữ đè nhau. Di chuyển hình học cần `set_geometry`. Tool geometry chỉ nhận polygon; chưa cung cấp thao tác reset về bố trí tự động mà native/UI đã hỗ trợ qua `points: None`. Cũng chưa có tool transform tương ứng với trải nghiệm resize/rotate trong canvas.

Bằng chứng: `crates/koharu-app/src/commands/agent/host.rs:289`, `:311`, `:842`, `:866`; `crates/koharu-app/src/commands/project.rs:431`, `:970`, `:1091`; `packages/koharu/components/editor/Inspector.tsx:796`.

Hướng sửa: mô tả rõ việc đổi cây layer và đổi vị trí trên ảnh; thao tác reorder trang phải có đích chapter rõ và trả membership sau sửa. Dùng khả năng transform/reset hiện có khi mở rộng tool, không thêm một đường chỉnh hình học riêng.

### 7. Thiếu ngữ cảnh selection và tên chapter trong progress

Agent biết active_page từ native ProjectInfo, nhưng UI chỉ gửi message và tên project. Selected layer IDs chưa được gửi. Câu “sửa chữ đang chọn” vì vậy không có ID selection để ánh xạ chắc chắn.

Progress chỉ có tên project, page label, element, tool và stage; không có chapter title hoặc vị trí trang. Khi nhiều chapter cùng có trang `007`, Activity chưa phân biệt được “Chapter nào — trang 007”. Bộ đếm changes là số tool edit thành công, không phải số trang đã hoàn thành.

Bằng chứng: `packages/koharu/components/editor/AgentPanel.tsx:527`; `packages/koharu/lib/store.ts:58`; `crates/koharu-app/src/commands/agent/session.rs:54`; `crates/koharu-app/src/commands/agent/host.rs:324`.

Hướng sửa: chụp selection đúng lúc gửi, gắn với project/page được xác minh; agent chạy nền dùng selection của yêu cầu đó, không đọc selection của project mới. Progress bổ sung chapter và số thứ tự trang thực tế.

## Kiểm kê toàn bộ tool

| Tool | Đánh giá với thiết kế hiện tại |
| --- | --- |
| inspect_project | Đã có pagination/budget và project target đúng; thiếu roster/tên/thứ tự chapter và vị trí trang. |
| inspect_page | Đã đọc semantic layers/analysis regions; chapter chỉ là ID, chưa có resolved layout/font metrics. |
| list_fonts | Đọc font family hiện có; chưa có style/weight hay kiểm tra glyph của từng font. Đây là khả năng còn thiếu, không phải bằng chứng font đang hỏng. |
| view_page | Render snapshot đúng project được giữ, không đổi canvas project khác; chưa có crop/zoom và metadata chapter. |
| rename_page | Dùng native commit; đổi label không thay thế được chapter/page ordinal rõ ràng. |
| move_page | Native cập nhật chapter membership; tool vẫn chỉ nhận index toàn project. |
| delete_pages | Native cập nhật membership và loại chapter rỗng; tool chưa mô tả tác dụng lên chapter hoặc ràng buộc phạm vi run. |
| add_text_box | Thêm paragraph text box bằng native; không phải toàn bộ công cụ tạo text/paint của canvas. |
| set_source_text | Dùng semantic text layer và giữ provenance qua native; chưa có run page-scope guard. |
| set_translation | Có nguy cơ thiếu text trở thành xóa bản dịch. |
| set_typography | Đầy đủ trường cơ bản, nhưng replacement đòi gửi lại cả bộ định dạng. |
| set_geometry | Sửa polygon của text layer; chưa mở reset automatic hoặc interface transform của canvas. |
| set_visibility | Thực hiện qua native; selection và run page-scope chưa được nối. |
| delete_elements | Native xử lý text group/content; chưa có run page-scope guard. |
| move_element | Đổi cây layer, không phải tọa độ hộp chữ. |
| run_pipeline | Shared pipeline/admission/target đúng; thiếu explicit chapter/range, resume và chapter summary workflow. |

Các chức năng import truyện, export website, metadata/FAQ xuất bản, paint/erase và Undo bằng tool cũng chưa được mở cho Editor Agent. Đây là các khả năng riêng chưa được cung cấp; không cần mở tất cả để sửa font/chữ trong phạm vi chapter. Undo vẫn dùng được qua giao diện.

## Phần đã thích nghi

- Mỗi run giữ đúng ProjectLease/instance; mọi tool host đọc và ghi vào target đó thay vì project đang hiển thị.
- Mutation dùng native project operations, revision check, commit và Undo; kết quả chỉ đồng bộ canvas nếu project đó đang hiển thị.
- Agent-requested pipeline chờ cùng processing admission lane, được Stop khi chờ/chạy; không cần tách harness cho chapter.
- Context và semantic inspection đã phân trang, có byte budget; không đưa toàn bộ project/layer vào prompt mỗi lượt.
- Các backend dùng cùng định nghĩa tool; lỗi tool được gửi lại cho agent và hiển thị trong tiến trình.

## Thứ tự nên sửa và điều kiện kiểm tra

1. Sửa nhận tham số trước: thiếu text không được xóa bản dịch; field sai không được bỏ qua; thiếu phạm vi không được tự mở rộng thành cả project. Giữ lời gọi lỗi không làm thay đổi revision/nội dung.
2. Bổ sung chapter/ordinal/range vào interface chung. Fixture có ít nhất hai chapter cùng nhãn trang, chapter rỗng, range qua ranh giới chapter và trang trước điểm bắt đầu được bảo vệ.
3. Nối resume và chapter summary/setting/wiki vào workflow hiện tại, giữ binding project và lane chung; không chạy lại trang đã xong và không bỏ qua summary toggle khi yêu cầu workflow chapter.
4. Làm tool sửa font/layout phù hợp yêu cầu nhỏ: đổi một thuộc tính giữ các thuộc tính khác, phân biệt reorder với transform, hỗ trợ reset automatic.
5. Truyền selection lúc gửi và hiển thị chapter/page trong Activity; kiểm tra agent A tiếp tục đúng target trong khi UI đang chọn project B.

Đây là kết quả rà soát, chưa phải bản triển khai những thay đổi trên. Không có model run hoặc native-window nghiệm thu mới trong lần này.
