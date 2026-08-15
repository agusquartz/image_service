[← Back to README](../README.md)

# Client Examples

The examples below use:

```text
Base URL:    http://127.0.0.1:3000
Namespace:   products
Resource ID: 845
Slot:        1
Image:       ./photo.jpg
```

If API authentication is disabled, remove the `Authorization` header from upload examples.

---

## cURL

### Upload

```bash
curl \
  -X POST \
  "http://127.0.0.1:3000/api/images/products/845/1" \
  -H "Authorization: Bearer change-me" \
  -F "image=@./photo.jpg"
```

Example response:

```json
{
  "namespace": "products",
  "resource_id": "845",
  "slot": 1,
  "url": "/api/images/products/845/1",
  "size_bytes": 193284
}
```

### Fetch

Save the image:

```bash
curl \
  "http://127.0.0.1:3000/api/images/products/845/1" \
  --output product.webp
```

Inspect the headers:

```bash
curl \
  -I \
  "http://127.0.0.1:3000/api/images/products/845/1"
```

---

## JavaScript Fetch API

These examples use the standard Fetch API available in modern browsers and recent Node.js versions.

### Upload from a browser file input

HTML:

```html
<input id="image" type="file" accept="image/*">
```

JavaScript:

```javascript
const input = document.querySelector("#image");
const file = input.files[0];

const form = new FormData();
form.append("image", file);

const response = await fetch(
  "http://127.0.0.1:3000/api/images/products/845/1",
  {
    method: "POST",
    headers: {
      Authorization: "Bearer change-me",
    },
    body: form,
  },
);

if (!response.ok) {
  const error = await response.json();
  throw new Error(error.error);
}

const result = await response.json();

console.log(result);
```

Do not manually set `Content-Type` when sending `FormData`.

The Fetch implementation generates the multipart boundary automatically.

### Fetch an image

```javascript
const response = await fetch(
  "http://127.0.0.1:3000/api/images/products/845/1",
);

if (!response.ok) {
  throw new Error(`HTTP ${response.status}`);
}

const blob = await response.blob();
const objectUrl = URL.createObjectURL(blob);

document.querySelector("img").src = objectUrl;
```

For a public image URL used directly in HTML:

```html
<img
  src="http://127.0.0.1:3000/api/images/products/845/1"
  alt="Product"
/>
```

---

## Java

The following example uses `java.net.http.HttpClient`.

### Upload

```java
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.UUID;

public class UploadImage {
    public static void main(String[] args) throws Exception {
        var client = HttpClient.newHttpClient();

        var imagePath = Path.of("./photo.jpg");
        var imageBytes = Files.readAllBytes(imagePath);

        var boundary = "----ImageServiceBoundary" + UUID.randomUUID();

        var prefix = (
            "--" + boundary + "\r\n" +
            "Content-Disposition: form-data; name=\"image\"; filename=\"photo.jpg\"\r\n" +
            "Content-Type: image/jpeg\r\n\r\n"
        ).getBytes(StandardCharsets.UTF_8);

        var suffix = (
            "\r\n--" + boundary + "--\r\n"
        ).getBytes(StandardCharsets.UTF_8);

        var body = new byte[
            prefix.length + imageBytes.length + suffix.length
        ];

        System.arraycopy(
            prefix,
            0,
            body,
            0,
            prefix.length
        );

        System.arraycopy(
            imageBytes,
            0,
            body,
            prefix.length,
            imageBytes.length
        );

        System.arraycopy(
            suffix,
            0,
            body,
            prefix.length + imageBytes.length,
            suffix.length
        );

        var request = HttpRequest.newBuilder()
            .uri(
                URI.create(
                    "http://127.0.0.1:3000/api/images/products/845/1"
                )
            )
            .header(
                "Authorization",
                "Bearer change-me"
            )
            .header(
                "Content-Type",
                "multipart/form-data; boundary=" + boundary
            )
            .POST(
                HttpRequest.BodyPublishers.ofByteArray(body)
            )
            .build();

        var response = client.send(
            request,
            HttpResponse.BodyHandlers.ofString()
        );

        System.out.println(response.statusCode());
        System.out.println(response.body());
    }
}
```

For larger production clients, a higher-level HTTP library with native multipart support may be preferable.

### Fetch

```java
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.file.Files;
import java.nio.file.Path;

public class FetchImage {
    public static void main(String[] args) throws Exception {
        var client = HttpClient.newHttpClient();

        var request = HttpRequest.newBuilder()
            .uri(
                URI.create(
                    "http://127.0.0.1:3000/api/images/products/845/1"
                )
            )
            .GET()
            .build();

        var response = client.send(
            request,
            HttpResponse.BodyHandlers.ofByteArray()
        );

        if (response.statusCode() != 200) {
            throw new RuntimeException(
                "HTTP " + response.statusCode()
            );
        }

        Files.write(
            Path.of("product.webp"),
            response.body()
        );
    }
}
```

---

## Python

The following examples use the popular `requests` package.

Install it if necessary:

```bash
python -m pip install requests
```

### Upload

```python
from pathlib import Path

import requests

url = (
    "http://127.0.0.1:3000"
    "/api/images/products/845/1"
)

headers = {
    "Authorization": "Bearer change-me",
}

path = Path("./photo.jpg")

with path.open("rb") as image:
    response = requests.post(
        url,
        headers=headers,
        files={
            "image": (
                path.name,
                image,
                "image/jpeg",
            )
        },
        timeout=30,
    )

response.raise_for_status()

print(response.json())
```

### Fetch

```python
from pathlib import Path

import requests

url = (
    "http://127.0.0.1:3000"
    "/api/images/products/845/1"
)

response = requests.get(
    url,
    timeout=30,
)

response.raise_for_status()

Path("product.webp").write_bytes(
    response.content
)
```

---

## Rust Client

The following examples use `reqwest`.

Example dependencies:

```toml
[dependencies]
reqwest = {
    version = "0.12",
    features = [
        "json",
        "multipart",
    ],
}
serde_json = "1"
tokio = {
    version = "1",
    features = ["full"],
}
```

### Upload

```rust
use reqwest::{
    multipart,
    Client,
};

#[tokio::main]
async fn main()
    -> Result<(), Box<dyn std::error::Error>>
{
    let client =
        Client::new();

    let bytes =
        tokio::fs::read(
            "./photo.jpg"
        )
        .await?;

    let part =
        multipart::Part::bytes(bytes)
            .file_name("photo.jpg")
            .mime_str("image/jpeg")?;

    let form =
        multipart::Form::new()
            .part(
                "image",
                part,
            );

    let response =
        client
            .post(
                "http://127.0.0.1:3000/api/images/products/845/1"
            )
            .bearer_auth(
                "change-me"
            )
            .multipart(form)
            .send()
            .await?;

    let status =
        response.status();

    let body =
        response.text().await?;

    println!("{status}");
    println!("{body}");

    Ok(())
}
```

### Fetch

```rust
use reqwest::Client;

#[tokio::main]
async fn main()
    -> Result<(), Box<dyn std::error::Error>>
{
    let client =
        Client::new();

    let response =
        client
            .get(
                "http://127.0.0.1:3000/api/images/products/845/1"
            )
            .send()
            .await?
            .error_for_status()?;

    let bytes =
        response
            .bytes()
            .await?;

    tokio::fs::write(
        "product.webp",
        bytes,
    )
    .await?;

    Ok(())
}
```

---
