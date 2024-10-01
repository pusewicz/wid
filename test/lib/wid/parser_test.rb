module Wid
  class ParserTest < Test
    def test_parse
      tokens = Lexer.tokenize(<<~WID)
        1 + 2.0
      WID

      parser = Parser.new(tokens)
      ast = parser.parse

      assert_empty(parser.errors)
      assert_equal(Nodes::Program, ast.class)
    end

    def test_parse_nil
      tokens = Lexer.tokenize(<<~WID)
        nil
      WID

      parser = Parser.new(tokens)
      ast = parser.parse

      assert_empty(parser.errors)
      assert_equal(Nodes::Nil, ast.children.first.class)
    end
  end
end
