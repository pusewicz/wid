module Wid
  class ParserLiteralsTest < ParserTest
    def test_parse_integer_number
      ast = parse("1")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::NumericLiteral,
              value: 1
            }
          }
        ]
      }, ast)
    end

    def test_parse_float_number
      ast = parse("1.53")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::NumericLiteral,
              value: 1.53
            }
          }
        ]
      }, ast)
    end

    def test_parse_double_quote_string
      ast = parse('"hello"')

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::StringLiteral,
              value: "hello"
            }
          }
        ]
      }, ast)
    end

    def test_parse_single_quote_string
      ast = parse("'hello'")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::StringLiteral,
              value: "hello"
            }
          }
        ]
      }, ast)
    end

    def test_parse_number_as_string
      ast = parse('"1"')

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::StringLiteral,
              value: "1"
            }
          }
        ]
      }, ast)
    end

    def test_parse_string_with_spaces
      ast = parse('"hello world"')

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::StringLiteral,
              value: "hello world"
            }
          }
        ]
      }, ast)
    end
  end
end
