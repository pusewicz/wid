# frozen_string_literal: true

module Wid
  class LexerTest < Test
    def test_generate
      tokens = Lexer.tokenize(<<~WID)
        1 + 2.0
      WID
      ast = Parser.parse(tokens)
      output = Codegen.generate(ast)

      assert_equal(<<~C, output)
        #include <stdio.h>

        int main() {
          (1 + 2.0);

          return 0;
        }
      C
    end
  end
end
