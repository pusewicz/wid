module Wid
  class ParserTest < Test
    def test_parse
      tokens = Lexer.tokenize(<<~WID)
        1 + 2.0
      WID

      ast = Parser.parse(tokens)

      assert_equal(Nodes::Program, ast.class)
    end
  end
end
