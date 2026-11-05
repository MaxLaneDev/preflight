# Generated for a reviewed release; publish only after real package acceptance.
require "securerandom"

class Preflight < Formula
  desc "Local build and test server with Web UI and MCP"
  homepage 'https://github.com/MaxLaneDev/preflight'
  url 'https://github.com/MaxLaneDev/preflight/releases/download/v0.1.4/preflight-0.1.4-macos-aarch64.tar.gz'
  version '0.1.4'
  sha256 'ae59ec4244352a8b43e92c99c0ca0f092579bf652bbbd5b892ccd19ca0a1f110'
  license "MIT"
  depends_on :macos
  depends_on arch: :arm64

  def install
    bin.install "preflight"
    pkgshare.install "manifest.json", "THIRD_PARTY_NOTICES.json", "licenses"
    (etc/"preflight").mkpath
    config = etc/"preflight/preflight.toml"
    unless config.exist?
      credential = etc/"preflight/admin-token"
      unless credential.exist?
        File.open(credential, File::WRONLY | File::CREAT | File::EXCL, 0600) do |file|
          file.write(SecureRandom.urlsafe_base64(48) + "\n")
        end
      end
      config.write <<~TOML
        version = 1
        bind = "127.0.0.1:4170"
        data_dir = "#{var}/preflight"
        allowed_roots = []
        admin_token_file = "#{credential}"
      TOML
    end
  end

  service do
    run [opt_bin/"preflight", "--config", etc/"preflight/preflight.toml", "serve"]
    keep_alive true
    log_path var/"log/preflight.log"
    error_log_path var/"log/preflight.log"
  end

  test do
    assert_match '"component":"preflight"', shell_output("#{bin}/preflight --version")
  end
end
