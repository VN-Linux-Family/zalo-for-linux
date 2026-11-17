# os-sys-info

[English](./README.md) | **Tiếng Việt**

## 📖 Giới thiệu

`os-sys-info` là một module Node.js native dùng để đọc thông tin hệ điều hành và tiến trình. Module cung cấp nhãn kiến trúc hệ điều hành, mã máy và thu thập snapshot của các tiến trình theo danh sách mã tiến trình (PID). Mỗi snapshot gồm thông tin tiến trình, bộ đếm thời gian CPU, số liệu bộ nhớ, cùng các mốc thời gian thực và đơn điệu. Lỗi truy xuất từng PID được trả về cùng các mẫu đọc thành công, nên một tiến trình không thể truy cập không làm mất kết quả của các tiến trình khác.

Các gói native được phân tích hướng đến macOS x64, macOS ARM64, Windows x64 và Windows x86. Chúng cùng cung cấp ba thao tác: `getOsArchitecture`, `getOsMachine` và `getProcessSnapshot`.

## 📁 Cấu trúc thư mục

```text
os-sys-info/
├── src/
│   └── lib.rs                 # Mã nguồn Rust native và các binding Node.js
├── target/
│   └── release/               # Tệp build release được tạo ra
├── Cargo.toml                 # Cấu hình package Rust và các dependency
├── README.vi.md               # Tài liệu hướng dẫn tiếng Việt
└── README.md                  # Tài liệu hướng dẫn tiếng Anh
```

- `src/` chứa mã nguồn thư viện Rust.
- `target/` chứa các artifact được tạo trong quá trình build; nội dung phụ thuộc cấu hình build.

## 🔎 Phân tích kỹ thuật

Phần này mô tả hợp đồng giao diện quan sát được của ba thao tác được export, gồm input và output; không mô tả cách triển khai bên trong.

### `getOsArchitecture`

**Input:** Không có.

**Output:** Nhãn kiến trúc hệ điều hành dưới dạng chuỗi. Giá trị `64-bit` được ghi nhận trong gói macOS ARM64. Cần xác nhận các nhãn cụ thể cho môi trường Windows trên máy chạy thực tế; kiến trúc của file gói tự nó không xác định được kiến trúc hệ điều hành.

### `getOsMachine`

**Input:** Không có.

**Output:** Mã máy dưới dạng chuỗi. Các nhãn quan sát được trên macOS là `x64` và `arm64`. Những giá trị khác hoặc trường hợp không xác định được máy có thể không trả về chuỗi. Nhãn này phản ánh loại máy do hệ điều hành báo cáo và không nhất thiết trùng với kiến trúc của tiến trình hiện tại.

### `getProcessSnapshot`

Trả về kết quả bất đồng bộ gồm các mẫu thông tin tiến trình và lỗi truy xuất.

#### Input

| Thuộc tính | Quy tắc |
|---|---|
| Danh sách PID | Bắt buộc và phải là danh sách số. Nếu sai kiểu, phát sinh lỗi với thông báo `expected pids: number[]`. |
| Mỗi PID | Phải là số nguyên dương nhỏ hơn 2³¹. Giá trị không hợp lệ sẽ bị bỏ qua. |
| PID trùng lặp | Mỗi PID chỉ được xử lý một lần; vị trí của lần xuất hiện đầu tiên được giữ lại. |
| Danh sách rỗng hoặc không có PID hợp lệ | Hoàn tất ngay với hai danh sách `samples` và `errors` rỗng. |

#### Output

Kết quả hoàn tất gồm hai danh sách:

| Trường | Nội dung |
|---|---|
| `samples` | Các bản ghi đọc thành công, mỗi PID truy xuất được có một bản ghi. |
| `errors` | Các bản ghi lỗi truy xuất. Mỗi bản ghi gồm `pid`, `op` (tên thao tác hệ thống bị lỗi) và `code` (mã lỗi của hệ điều hành). |

Mỗi phần tử trong `samples` có các trường sau:

| Trường | Kiểu kết quả | Ý nghĩa / đơn vị |
|---|---|---|
| `pid` | số | Mã tiến trình được yêu cầu. |
| `ppid` | số | Mã tiến trình cha. |
| `creation100ns` | số nguyên độ chính xác cao | Thời điểm tạo tiến trình, tính theo đơn vị 100 nano giây của hệ điều hành. |
| `kernelTime100ns` | số nguyên độ chính xác cao | Thời gian CPU tích lũy ở chế độ nhân, tính theo đơn vị 100 nano giây. |
| `userTime100ns` | số nguyên độ chính xác cao | Thời gian CPU tích lũy ở chế độ người dùng, tính theo đơn vị 100 nano giây. |
| `workingSet` | số | Dung lượng bộ nhớ cư trú hiện tại, tính bằng byte. |
| `privateBytes` | số | Dung lượng bộ nhớ riêng, tính bằng byte. |
| `wallUnixMs` | số | Mốc thời gian thực Unix, tính bằng mili giây. |
| `monotonicMs` | số | Mốc thời gian đơn điệu, tính bằng mili giây; phù hợp để đo thời gian đã trôi qua trong cùng một phiên đo. |

Các trường thời gian CPU là bộ đếm tích lũy của tiến trình. Có thể ước lượng mức sử dụng CPU giữa hai lần lấy mẫu bằng công thức:

```text
CPU% = 100 × ((K₂ − K₁) + (U₂ − U₁)) / (T₂ − T₁)
```

Trong đó `K` là `kernelTime100ns`, `U` là `userTime100ns`, còn `T` là thời gian thực đã đổi sang cùng đơn vị 100 nano giây. Một lõi CPU được sử dụng tối đa tương ứng 100%; kết quả có thể vượt 100% trên máy nhiều lõi.

#### Quy tắc kết quả

1. PID không hợp lệ hoặc bị trùng không tạo phần tử trong `samples` hay `errors`.
2. Mỗi PID hợp lệ được truy xuất riêng. Truy xuất thành công tạo một `sample`; truy xuất thất bại tạo một `error` tương ứng.
3. PID không thể truy cập không làm mất các kết quả thành công khác trong cùng yêu cầu.
4. Các mẫu thành công giữ thứ tự của các PID duy nhất trong input. Các lỗi giữ thứ tự xử lý PID.
5. Một số chỉ số có thể không khả dụng nếu hệ điều hành từ chối truy cập hoặc tiến trình kết thúc trong lúc thu thập. Xem `errors` để biết thao tác bị lỗi và mã lỗi tương ứng.

## 📝 Ghi chú tương thích

- Các gói được phân tích có cùng tên thao tác và nhãn trường. Việc phân loại kiến trúc và mã máy trên Windows chưa được xác minh bằng cách chạy gói trên Windows; không suy luận giá trị trả về chỉ từ hậu tố tên file của gói.
- Đặc tả này mô tả hành vi suy ra từ phân tích tĩnh REA. Giá trị thực tế phụ thuộc hệ điều hành, quyền truy cập và trạng thái tiến trình tại thời điểm gọi.
