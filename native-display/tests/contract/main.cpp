// contract_tests のエントリポイント。
//
// unit_tests との違いは **リンクするもの**:
//   unit_tests     : ambient_core だけ
//   contract_tests : ambient_core + ambient_transport
//
// ★実サーバにも本番 API にも繋がないこと。fixture と Fake だけを使う。

#define DOCTEST_CONFIG_IMPLEMENT_WITH_MAIN
#include <doctest.h>
