// C entry point around HLSLcc's C++ API, forwarding reflection callbacks to C
// function pointers.
#include "hlslcc.h"

#include <cstdint>
#include <cstring>

extern "C" {

struct HlslccCallbacks {
  void *user;
  void (*diagnostic)(void *user, const char *message, int line, int is_error);
  void (*input)(void *user, const char *name, int bind_index);
  void (*constant_buffer)(void *user, const char *name, size_t size,
                          size_t member_count);
  void (*constant)(void *user, const char *name, int offset, int type, int rows,
                   int columns, int is_matrix, int array_size, int is_used);
  void (*constant_buffer_binding)(void *user, const char *name, int bind_index);
  void (*texture_binding)(void *user, const char *name, int bind_index,
                          int sampler_index, int multisampled, int dimension,
                          int is_uav);
  void (*buffer_binding)(void *user, const char *name, int bind_index,
                         int is_uav);
  void (*source)(void *user, const char *source, size_t length);
};

} // extern "C"

namespace {

struct Forwarder : HLSLccReflection {
  const HlslccCallbacks *cb;

  explicit Forwarder(const HlslccCallbacks *callbacks) : cb(callbacks) {}

  void OnDiagnostics(const std::string &error, int line,
                     bool isError) override {
    cb->diagnostic(cb->user, error.c_str(), line, isError);
  }
  void OnInputBinding(const std::string &name, int bindIndex) override {
    cb->input(cb->user, name.c_str(), bindIndex);
  }
  bool OnConstantBuffer(const std::string &name, size_t bufferSize,
                        size_t memberCount) override {
    cb->constant_buffer(cb->user, name.c_str(), bufferSize, memberCount);
    return true;
  }
  bool OnConstant(const std::string &name, int bindIndex,
                  SHADER_VARIABLE_TYPE cType, int rows, int cols, bool isMatrix,
                  int arraySize, bool isUsed) override {
    cb->constant(cb->user, name.c_str(), bindIndex, (int)cType, rows, cols,
                 isMatrix, arraySize, isUsed);
    return true;
  }
  void OnConstantBufferBinding(const std::string &name,
                               int bindIndex) override {
    cb->constant_buffer_binding(cb->user, name.c_str(), bindIndex);
  }
  void OnTextureBinding(const std::string &name, int bindIndex,
                        int samplerIndex, bool multisampled,
                        HLSLCC_TEX_DIMENSION dim, bool isUAV) override {
    cb->texture_binding(cb->user, name.c_str(), bindIndex, samplerIndex,
                        multisampled, (int)dim, isUAV);
  }
  void OnBufferBinding(const std::string &name, int bindIndex,
                       bool isUAV) override {
    cb->buffer_binding(cb->user, name.c_str(), bindIndex, isUAV);
  }
};

} // namespace

extern "C" void *hlslcc_dependencies_new() {
  return new GLSLCrossDependencyData();
}

extern "C" void hlslcc_dependencies_free(void *dependencies) {
  delete static_cast<GLSLCrossDependencyData *>(dependencies);
}

extern "C" int hlslcc_translate(const uint8_t *dxbc, uint32_t flags,
                                int language, uint32_t extensions,
                                void *dependencies,
                                const HlslccCallbacks *callbacks) {
  GlExtensions ext = {};
  ext.ARB_explicit_attrib_location = (extensions >> 0) & 1;
  ext.ARB_explicit_uniform_location = (extensions >> 1) & 1;
  ext.ARB_shading_language_420pack = (extensions >> 2) & 1;
  ext.OVR_multiview = (extensions >> 3) & 1;
  ext.EXT_shader_framebuffer_fetch = (extensions >> 4) & 1;

  HLSLccSamplerPrecisionInfo precisions;
  Forwarder reflection(callbacks);
  GLSLShader result;
  int ok = TranslateHLSLFromMem(
      reinterpret_cast<const char *>(dxbc), flags, (GLLang)language, &ext,
      static_cast<GLSLCrossDependencyData *>(dependencies), precisions,
      reflection, &result);
  if (ok) {
    callbacks->source(callbacks->user, result.sourceCode.data(),
                      result.sourceCode.size());
  }
  return ok;
}
