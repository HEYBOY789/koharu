# Rà soát Editor Agent với thiết kế hiện tại

Ngày: 31/10/2026. Source đã kiểm tra: `b4ce18dcbedc0f805145d8d2940e0189abd2349d`.

## Giữ lỗi theo target và kiểm tra hoàn tất — 03/11/2026

Nguyên nhân cảnh báo biến mất: trạng thái tiến độ cũ chỉ giữ lỗi của tool gần nhất, rồi xóa khi tool tiếp theo bắt đầu. Thành công ở trang khác và câu trả lời kết thúc của model chưa được đối chiếu với những lời gọi vẫn thất bại.

Native binding của từng run nay giữ danh sách lời gọi bị từ chối, ID phục hồi, tool, target trang/layer đã xác minh và số lần thất bại. UI chiếu danh sách này riêng với trang đang được nhắm tới; đổi tool, chuyển trang, suy luận hoặc kết thúc thất bại không làm mất cảnh báo. Activity giữ dấu cần chú ý. Metadata `retry_of` được khai báo trong schema; native chỉ bóc metadata này trước validator nghiệp vụ nghiêm ngặt. Không chấp nhận alias, đoán ID hoặc sửa JSON của model.

Chỉ lời gọi thành công có `retry_of` trỏ đúng lỗi, giữ tool và các target gốc mới gỡ được lỗi. Một thao tác thành công không gắn ID phục hồi cũng không tự xóa lỗi, kể cả khi cùng trang. Native từ chối retry đổi target trước khi mutation chạy; giữ chapter, offset, operation và ranh giới phạm vi đã biết. Retry không đổi dữ liệu vẫn có thể xác nhận phục hồi mà không tăng bộ đếm chỉnh sửa. Agent vẫn được đọc font, đọc và chỉnh trang khác để tiếp tục công việc; điều đó không chứng minh trang lỗi đã được xử lý.

AGY/Pi và Codex cùng kiểm tra `Host::ensure_complete` trước khi phát sự kiện hoàn tất. Còn lỗi thì tiếp tục trên cùng host/session/phạm vi, tối đa hai lượt nhắc phục hồi sau câu trả lời kết thúc. Ba lần thất bại của cùng lời gọi hoặc danh sách vượt giới hạn khiến run dừng với yêu cầu chú ý. Nếu đã gọi nhầm một tool không phù hợp, không chạy processing ngoài ý muốn chỉ để xóa lỗi: báo chưa hoàn tất. Dừng nội bộ dùng lease của run, không hủy control của người dùng hoặc job project khác. Native session có thêm chốt phòng thủ để không đổi lỗi còn tồn tại thành Finished.

Các kiểm tra hồi quy bao gồm lỗi trang 016 còn nguyên sau khi đọc/chỉnh trang khác, retry sai layer không thay đổi revision, retry no-op gỡ đúng lỗi, giữ phạm vi và model cố báo xong trước khi phục hồi. Fixture giao thức model có cả trường hợp biết sửa và tiếp tục bỏ qua; trường hợp bỏ qua phải kết thúc Failed. Đây là kiểm tra giao thức tổng hợp, không phải nghiệm thu tài khoản Pi. Giao diện được chụp ở cả trạng thái đang làm trang 017 và không phục hồi xong, kèm tên chapter dài. Kết quả AGY thực tế, build, commit và EXE được ghi tại validation của workspace website.

Giới hạn: danh sách thuộc run hiện tại trong bộ nhớ, giữ qua reconnect nhưng không phải hàng đợi phục hồi bền vững sau khởi động lại hoặc yêu cầu mới. Chốt này ngăn báo xong khi còn lời gọi lỗi; nó không tự đánh giá chất lượng hình ảnh hay chứng minh model đã rà từng trang của toàn bộ truyện. Không dừng hay chỉnh project đang chạy của người dùng để kiểm thử.

## Phục hồi lời gọi tool sai tham số — 02/11/2026

Hai ảnh tiếp theo ghi nhận `inspect_chapter` nhận `chapter_id` thay vì `chapter`, và `set_edit_scope` thiếu `first_page`/`last_page`. Đây là lời gọi bị validator từ chối trước khi thay đổi dữ liệu. Kiểm tra giao thức xác nhận `tools/list` chuyển nguyên schema, gồm các field bắt buộc; chưa có bằng chứng schema bị mất khi bridge chuyển cho AGY. Việc model tiếp tục điền sai không được xem là đã hết chỉ vì một lượt nghiệm thu trước đó thành công.

Phản hồi lỗi nay kèm `tool`, `received_arguments` nguyên nghĩa và `input_schema` lấy trực tiếp từ định nghĩa tool đang dùng, cùng `ok:false` và lỗi native. AGY/Pi qua MCP và Codex dùng chung cách tạo phản hồi này. Chuỗi JSON không hợp lệ được giữ nguyên dưới dạng chuỗi, không tự sửa thành một payload khác. Không đoán ID, chấp nhận alias `chapter_id`, chọn phạm vi mặc định hoặc tự rộng phạm vi để qua lỗi. UI tiếp tục lấy riêng trường `error` cho cảnh báo ngắn và không hiển thị nguyên schema.

Mô tả `inspect_chapter` nêu rõ key `chapter` chứa ID từ `inspect_chapters.chapters[].id`; mô tả `set_edit_scope` nêu rõ cần cả hai ID trang và `{}` không hợp lệ. Kiểm tra bridge tái hiện hai payload sai, xác minh lỗi không tính là chỉnh sửa, schema/arguments không bị đổi và các lời gọi đã sửa được chuyển đúng tới host. Kiểm tra model dùng fixture native gồm 16 trang ở Chapter 1 và một trang Chapter 2, với lỗi validator được tiêm rõ ràng trong host thử nghiệm để xác minh khả năng gọi lại sau khi bị từ chối. Lượt yêu cầu model tự tạo payload sai trước đó không kích hoạt lỗi cần thử, nên không được tính là nghiệm thu phục hồi.

Lượt AGY `gemini-3.8-flash-high` có tiêm lỗi đã qua: từng tool bị từ chối rồi được gọi lại thành công, phạm vi bắt đầu đúng trang 016 của Chapter 1 và kết thúc ở trang cuối project. Sau đó native xác minh đủ thuộc tính đã lưu và có render sau chỉnh sửa. Đây là kiểm tra phục hồi lỗi được tiêm có kiểm soát, không phải khẳng định model tự tái tạo hai payload sai trong ảnh.

Giới hạn: đây là tăng chất lượng phản hồi để model phục hồi. Nó không bảo đảm model không bao giờ gọi sai hoặc tự hoàn thành mọi lượt thực tế. Kết quả build và bàn giao được ghi trong validation của workspace website. Không chỉnh hay dừng lượt agent/project đang chạy của người dùng để làm bài thử.

## Kiểm tra thuộc tính layer và vị trí agent — 02/11/2026

Lần kiểm tra này đối chiếu trực tiếp `TypeInspector`, `LayerEditor`, cây Layers và các tool native. Layer mutations dùng chung một bộ lệnh đã parse cho ứng dụng và fixture nghiệm thu; fixture không còn giả lập thành công bằng cách tăng bộ đếm. Các thay đổi vẫn giữ project lease, revision check, phạm vi trang, patch native, Undo và đồng bộ canvas của ứng dụng.

| Điều khiển | Tool và biểu diễn thực tế | Kiểm tra native |
| --- | --- | --- |
| Font | `list_fonts` trả family, faces, weight/range/style và metadata giống Properties; `set_typography.preferred_font` | Đổi font giữ các thuộc tính khác, đọc lại sau khi mở project |
| Độ đậm, kiểu chữ | `font_weight` là số 1–1000; `font_style` là `normal/italic/oblique` | Weight 400/700; cả ba style; từ chối `"bold"`, weight 0/1001 |
| Cỡ chữ, Auto | `size` là pixel dương, `auto_fit` là boolean; `size:null` bỏ cỡ cố định | Cỡ cố định, Auto, giữ thuộc tính bỏ qua, từ chối size không hợp lệ |
| Màu chữ | `color:[r,g,b,a]`, mỗi kênh 0–255 | Ghi/đọc RGBA, xóa override; từ chối hex hoặc kênh ngoài khoảng |
| Border | `stroke_color` RGBA và `stroke_width >= 0`; alpha 0 hoặc width 0 tắt viền | Màu/alpha/độ dày và xóa override; từ chối độ dày âm |
| Căn lề, hướng chữ | Dùng cùng giá trị native: `Start/Center/End/Justify`, `Horizontal/Vertical`; `null` là Auto/inherit | Mọi lựa chọn, xóa override; input tool và output inspection thống nhất chữ hoa/thường |
| Vị trí, kích thước hộp chữ | `set_geometry` dùng polygon trong tọa độ trang | Ghi/đọc điểm; reset bằng `null` chỉ được cho layout có vùng tự động |
| Ẩn/hiện, opacity | `set_visibility`, opacity 0–1 | Thay riêng từng thuộc tính giữ phần còn lại; từ chối opacity 50 |
| Nội dung, xuống dòng | `set_source_text`, `set_translation` | Ghi/đọc text và xuống dòng; `text:null` xóa translation có chủ đích |
| Thêm, xóa, sắp xếp layer | `add_text_box`, `delete_elements`, `reorder_element` | Tạo hộp có góc, đổi thứ tự cây, xóa và Undo |

`font_weight:"bold"` vẫn bị từ chối: native không sửa đoán payload. Schema và hướng dẫn nay ghi rõ số 700 cho bold, cách chọn face hỗ trợ, RGBA và đơn vị. Căn lề/hướng chữ trước đó nhận lowercase trong khi inspection trả tên enum native có chữ hoa; hợp đồng tool đã được thống nhất với inspection, không giữ alias cũ.

Agent/Activity hiển thị trang đang được tool nhắm tới gồm tên chapter, vị trí `3/50` và nhãn trang. Khi agent đọc font/project hoặc chuyển sang suy luận, giao diện ghi rõ **trang gần nhất**. Trạng thái này được giữ qua các tool không có target, nhưng reset khi bắt đầu run/project khác. Không tự chuyển canvas đang được người dùng thao tác. Bộ đếm chỉ tăng khi commit đổi revision; gửi lại cùng giá trị không được tính là một chỉnh sửa mới.

Kiểm tra tập trung gồm native settings roundtrip, Undo, từ chối payload sai, phạm vi trang, no-op và persistence; UI kiểm tra target hiện tại/trang gần nhất và hai project song song. Lượt nghiệm thu AGY/Gemini dùng project native tạm, font catalog thật và renderer native để kiểm tra model gửi tham số, lưu layer và xem ảnh trước/sau. Kết quả chạy/build/bàn giao cuối được ghi trong `F:\web_truyen\docs\VALIDATION.md`.

Giới hạn: đây không phải kết luận mọi trang trong project người dùng đã được dàn chữ đẹp. Line height, letter/word spacing, inset, brush/erase/freehand và thao tác pan không có tool Editor Agent tương ứng; một số giá trị spacing còn do renderer quyết định. Geometry polygon không phải tool kéo/rotate canvas theo resolved layout metrics. Chưa mở tool Undo cho agent; Undo vẫn có trong ứng dụng. Các luồng chapter summary/wiki và xuất bản giữ phạm vi như ghi ở phần trước.

## Cập nhật triển khai — 02/11/2026

Phần bên dưới lưu kết quả rà soát ban đầu. Các thay đổi sau đã được triển khai cho sửa layer/font theo chapter:

- AGY khai báo trực tiếp plugin MCP của phiên chạy trong agent Markdown. Lượt kiểm tra thật với AGY 1.4.12 / `gemini-3.8-flash-high` đã đọc chapter, xác định trang thứ 3, chọn khoảng trang, đọc font/layer và gửi `set_typography` với ID layer cùng tên font. Trước khi khai báo plugin trực tiếp, cùng lượt kiểm tra báo không có tool Koharu. Không cài plugin toàn cục hoặc đổi thiết lập tài khoản. Cách khai báo customization theo agent được mô tả trong [tài liệu custom agents của Google](https://antigravity.google/docs/subagents/).
- Mọi struct nhận tham số từ agent từ chối field không hỗ trợ. `set_translation.text` và `set_geometry.points` bắt buộc có mặt; `null` là yêu cầu xóa/reset có chủ đích. Lời gọi thiếu ID trả hợp đồng tool và hướng dẫn đọc ID rồi thử lại; native không đoán ID hoặc sửa payload.
- `set_typography` cập nhật các thuộc tính được gửi. Thuộc tính bỏ qua được giữ lại; `null` xóa override tương ứng. Các thay đổi vẫn đi qua thao tác native, revision check và Undo. Đổi font không yêu cầu gửi lại màu, viền, cỡ chữ, căn chỉnh hoặc auto-fit.
- `inspect_chapters` liệt kê cả chapter rỗng; `inspect_chapter` trả trang theo thứ tự trong chapter. Page inspection/overview trả chapter ID/tên/thứ tự, vị trí trang 1-based trong chapter và toàn project. Pagination và giới hạn phản hồi 64 KiB được giữ lại. Context dùng chung một roster đã đọc cho mỗi batch.
- Trước khi sửa, agent chọn `set_edit_scope(first_page, last_page)` bằng ID trang đầu/cuối, bao gồm cả hai đầu. Mỗi lượt yêu cầu có phạm vi riêng. Mutation, đích reorder trang/layer và pipeline đều kiểm tra phạm vi đã chọn. `run_pipeline` yêu cầu page hoặc element IDs tường minh, không tự biến danh sách rỗng thành cả project; `resume` mặc định bật. Cơ chế này chặn thao tác ngoài phạm vi agent đã chọn; việc hiểu đúng câu tiếng Việt và chọn phạm vi tương ứng vẫn là trách nhiệm của model.
- `move_element` được thay bằng `reorder_element`, chỉ đổi cây layer. Di chuyển/resize chữ trên ảnh dùng `set_geometry`; reset geometry tự động dùng `points: null` và vẫn tuân theo kiểm tra native. `move_page` vẫn dùng index toàn project, mô tả rõ tác dụng lên chapter và trả lại membership/vị trí sau khi đổi.
- Giao diện gửi page/layer selection tại thời điểm gửi yêu cầu. Native xác minh selection thuộc đúng project/trang trước khi truyền cho model. Tiến trình hiện chapter, vị trí trang trong chapter và label để phân biệt các trang trùng tên. Project binding/chạy nền và Stop dùng chủ sở hữu hiện có.

Lượt theo dõi cùng ngày: yêu cầu sửa chữ chồng, viền và font size là chỉnh presentation của layer hiện có. Hướng dẫn agent và mô tả `run_pipeline` nay phân biệt rõ thao tác này với tạo lại detection/OCR/translation/inpainting; layer edit tự lưu và cập nhật canvas, không cần gọi pipeline để refresh. Lỗi tham số chỉ trả thông tin ngắn cùng tên các trường bắt buộc; schema đầy đủ vẫn có trong định nghĩa tool, không bị đổ vào khung tiến trình. Một lượt AGY/Gemini Flash thật đã gửi đủ operation/page IDs cho yêu cầu OCR tường minh. Lượt khác dùng prompt sửa toàn project của người dùng với fixture ảnh trước/sau đã gọi tool typography, xem lại ảnh và không gọi pipeline. Hai lượt này xác minh chọn tool/tham số bằng schema native, không phải nghiệm thu chất lượng dàn chữ trên toàn bộ truyện thật. Người dùng xác nhận workflow dịch và agent edit ở hai project khác nhau; cơ chế ownership đó giữ nguyên.

Bổ sung sau ảnh `inspect_project` có field `unknown`: các tool đọc chỉ có `offset`/`limit` cho phép `{}` để dùng mặc định, nên cấm mọi lời gọi rỗng trong hướng dẫn sửa lỗi là không nhất quán. Diagnostic nay trả tên field được nhận, field bắt buộc nếu có, và nói rõ `{}` hợp lệ khi tất cả field là tùy chọn. Tool cần ID/operation vẫn từ chối thiếu field; field lạ vẫn bị chặn. Agent không được tự bịa placeholder `unknown`. Giao diện hiển thị lời gọi tool bị từ chối như một cảnh báo khi run vẫn đang chạy, giữ Stop và chi tiết lỗi; cảnh báo tự hết khi native có bước tiếp theo hoặc kết thúc. Một lượt AGY/Gemini thật đã nhận lỗi native được tiêm có kiểm soát, đọc lại bằng `offset`/`limit` hợp lệ rồi hoàn thành chỉnh font. Điều này xác minh đường phục hồi; không có bằng chứng rằng lượt thật của người dùng đã kẹt, vì người dùng đã tắt lượt đó ngay khi thấy lỗi.

29 kiểm tra tập trung đã qua: 11 native, 6 CLI/bridge, 6 UI/workspace, 5 localization và một lượt AGY/Gemini thật. Bốn kiểm tra hợp đồng thất bại trước khi sửa. Project native tạm xác minh phạm vi qua hai chapter có label trùng nhau, bảo vệ trang trước điểm bắt đầu, cập nhật font giữ thuộc tính khác, xóa override có chủ đích, Undo và selection cũ bị từ chối. Lượt AGY thật dùng các schema được xuất từ chính tool native với fixture trong bộ kiểm tra CLI hiện có; không chỉnh project người dùng và không phải nghiệm thu toàn cửa sổ native. TypeScript và frontend export cũng đã qua. Chi tiết log/build/bàn giao được ghi trong hồ sơ validation của workspace website.

Các khả năng riêng **chưa mở cho Editor Agent**: đọc/chỉnh nội dung summary/setting/wiki và chạy workflow summarize-before-translate của chapter; transform canvas có resolved font/layout metrics; paint/erase, import/export xuất bản và Undo bằng tool. Workflow Chapter trong giao diện vẫn sở hữu summarize/wiki. Các phát hiện tương ứng bên dưới là lịch sử và chỉ phần còn lại này chưa được triển khai.

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
