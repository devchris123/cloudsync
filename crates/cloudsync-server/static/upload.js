// Browser uploader for CloudSync.
//
// Flow per file:
//   1. Stream the file through BLAKE3 (hash-wasm) to get total_hash.
//   2. POST /api/v1/uploads to allocate an upload_id.
//   3. PUT each 4 MiB chunk in parallel (concurrency 5) via XHR so we can
//      drive a per-file <progress> bar from upload.onprogress.
//   4. POST /api/v1/uploads/{id}/finalize.
//   5. Refresh the file row list via HTMX.
//
// Fetch is intentionally NOT used for chunk PUTs: it gives no upload progress
// events. The init/finalize/list-refresh calls are small enough to use fetch.

(() => {
    const CHUNK_SIZE = 4 * 1024 * 1024; // 4 MiB — stays under the 5 MiB per-chunk body limit.
    const CONCURRENCY = 5; // Matches the Rust client's default batch size.

    const dropzone = document.getElementById('dropzone');
    const picker = document.getElementById('filepicker');
    const pickButton = document.getElementById('pickbutton');
    const uploadsList = document.getElementById('uploads');
    if (!dropzone || !picker || !pickButton || !uploadsList) return;

    const prefix = dropzone.dataset.prefix || '';

    // --- drop / pick wiring ---

    pickButton.addEventListener('click', () => picker.click());
    picker.addEventListener('change', () => {
        if (picker.files) handleFiles(Array.from(picker.files));
        picker.value = '';
    });

    ['dragenter', 'dragover'].forEach((evt) =>
        dropzone.addEventListener(evt, (e) => {
            e.preventDefault();
            dropzone.classList.add('dragging');
        }),
    );
    ['dragleave', 'dragend', 'drop'].forEach((evt) =>
        dropzone.addEventListener(evt, () => dropzone.classList.remove('dragging')),
    );
    dropzone.addEventListener('drop', (e) => {
        e.preventDefault();
        if (e.dataTransfer?.files) handleFiles(Array.from(e.dataTransfer.files));
    });

    function handleFiles(files) {
        for (const file of files) uploadFile(file).catch((err) => console.error(err));
    }

    // --- progress row ---

    function makeRow(file) {
        const li = document.createElement('li');
        li.className = 'upload-row';
        li.innerHTML = `
            <span class="upload-name"></span>
            <progress max="100" value="0"></progress>
            <span class="upload-status">hashing…</span>
        `;
        li.querySelector('.upload-name').textContent = file.name;
        uploadsList.appendChild(li);
        return {
            setStatus(text) {
                li.querySelector('.upload-status').textContent = text;
            },
            setProgress(pct) {
                li.querySelector('progress').value = pct;
            },
            markError(message) {
                li.classList.add('error');
                li.querySelector('.upload-status').textContent = `error: ${message}`;
            },
            remove() {
                li.remove();
            },
        };
    }

    // --- hashing ---

    let blake3Promise = null;
    function getBlake3() {
        if (!blake3Promise) {
            // hashWasm is the UMD global exported by /static/hash-wasm.min.js
            blake3Promise = window.hashWasm.createBLAKE3();
        }
        return blake3Promise;
    }

    async function hashFile(file, onProgress) {
        const hasher = await getBlake3();
        hasher.init();
        let read = 0;
        for (let offset = 0; offset < file.size; offset += CHUNK_SIZE) {
            const slice = file.slice(offset, Math.min(offset + CHUNK_SIZE, file.size));
            const buf = new Uint8Array(await slice.arrayBuffer());
            hasher.update(buf);
            read += buf.byteLength;
            onProgress(file.size === 0 ? 1 : read / file.size);
        }
        return hasher.digest('hex');
    }

    // --- upload pipeline ---

    async function uploadFile(file) {
        const row = makeRow(file);
        try {
            const hashed = await hashFile(file, (p) => row.setProgress(Math.round(p * 10)));
            row.setStatus('uploading 0%');

            const chunkCount = Math.max(1, Math.ceil(file.size / CHUNK_SIZE));
            const initResp = await fetch('/api/v1/uploads', {
                method: 'POST',
                credentials: 'same-origin',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                    path: joinPath(prefix, file.name),
                    total_size: file.size,
                    total_hash: hashed,
                    chunk_count: chunkCount,
                }),
            });
            if (!initResp.ok) throw new Error(`init failed (${initResp.status})`);
            const { upload_id: uploadId } = await initResp.json();

            // Per-chunk progress counters; we report the sum divided by total size.
            const loaded = new Array(chunkCount).fill(0);
            const updateProgress = () => {
                const total = loaded.reduce((a, b) => a + b, 0);
                const pct = file.size === 0 ? 100 : Math.min(99, Math.round((total / file.size) * 100));
                row.setProgress(pct);
                row.setStatus(`uploading ${pct}%`);
            };

            await runWithConcurrency(chunkCount, CONCURRENCY, async (i) => {
                const start = i * CHUNK_SIZE;
                const end = Math.min(start + CHUNK_SIZE, file.size);
                const slice = file.slice(start, end);
                await putChunk(uploadId, i, slice, (chunkLoaded) => {
                    loaded[i] = chunkLoaded;
                    updateProgress();
                });
            });

            row.setStatus('finalizing…');
            const finResp = await fetch(`/api/v1/uploads/${uploadId}/finalize`, {
                method: 'POST',
                credentials: 'same-origin',
            });
            if (!finResp.ok) throw new Error(`finalize failed (${finResp.status})`);

            row.setProgress(100);
            row.setStatus('done');
            // Splice the new row into the file table. The hidden prefix input lives
            // next to the search box, so HTMX includes it automatically — we still
            // pass it here in case it gets cleared by a search.
            window.htmx?.ajax('GET', `/partials/files?prefix=${encodeURIComponent(prefix)}`, '#file-rows');
            setTimeout(() => row.remove(), 1500);
        } catch (err) {
            console.error(err);
            row.markError(err.message || String(err));
        }
    }

    function putChunk(uploadId, index, blob, onProgress) {
        return new Promise((resolve, reject) => {
            const xhr = new XMLHttpRequest();
            xhr.open('PUT', `/api/v1/uploads/${uploadId}/chunks/${index}`, true);
            xhr.withCredentials = true;
            xhr.upload.onprogress = (e) => {
                if (e.lengthComputable) onProgress(e.loaded);
            };
            xhr.onload = () => {
                if (xhr.status >= 200 && xhr.status < 300) {
                    onProgress(blob.size);
                    resolve();
                } else {
                    reject(new Error(`chunk ${index} failed (${xhr.status})`));
                }
            };
            xhr.onerror = () => reject(new Error(`chunk ${index} network error`));
            xhr.send(blob);
        });
    }

    function runWithConcurrency(total, concurrency, worker) {
        let next = 0;
        const errors = [];
        async function runOne() {
            while (true) {
                const i = next++;
                if (i >= total) return;
                try {
                    await worker(i);
                } catch (err) {
                    errors.push(err);
                    return;
                }
            }
        }
        const workers = Array.from({ length: Math.min(concurrency, total) }, runOne);
        return Promise.all(workers).then(() => {
            if (errors.length) throw errors[0];
        });
    }

    function joinPath(prefix, name) {
        if (!prefix) return name;
        return prefix.endsWith('/') ? prefix + name : prefix + '/' + name;
    }
})();
