module Wid
  class ParserTest < Test
    def test_parse
      ast = parse("1 + 2.0")

      assert_equal(Nodes::Program, ast.class)
    end

    def test_parse_nil
      ast = parse("nil")

      assert_equal(Nodes::Nil, ast.children.first.class)
    end

    def test_function_call
      ast = parse("print(1)")

      assert_equal(Nodes::Program, ast.class)
      assert_equal(Nodes::FunctionCall, ast.children.first.class)
    end

    private

    def parse(input)
      tokens = Lexer.tokenize(input)
      Parser.parse(tokens)
    end
  end
end
