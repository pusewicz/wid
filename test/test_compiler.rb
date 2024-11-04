# frozen_string_literal: true

class CompilerTest < Wid::Test
  def test_compile
    expected = <<~C
      #include <stdio.h>

      #include "wid.h"

      int Wid__main() {
        printf("Hello, World!");;
        return 0;
      }

      int main() {
        return Wid__main();
      }
    C
    assert_equal expected, Wid::Compiler.compile("print 'Hello, World!'")
  end
end
