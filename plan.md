1. Update JSON payload limits in HTTP server endpoints to mitigate Deserialization DoS bombs.
   - Modify `src/http/server.rs` to include `actix_web::web::JsonConfig::default().limit(2 * 1024 * 1024)` in `create_app`, `create_server`, and `run_server` using `.app_data()`.
2. Add security headers and strict input limits to `tests/warden_json_payload.rs` integration test to ensure large payloads (> 2MB) correctly reject requests with a `413 Payload Too Large` status.
3. Update `.jules/warden.md` with new findings regarding Deserialization Bombs and the applied defense in Actix-web server configuration.
4. Complete pre commit steps to make sure proper testing, verifications, reviews and reflections are done.
5. Submit PR with Warden template.
