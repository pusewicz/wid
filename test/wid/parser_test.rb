module Wid
  class ParserTest
    def test_whitespace_skip
      ast = parse("    1 ")

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

    def test_comment_skip
      ast = parse("# This is a comment\n1")

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

    def test_comment_after_expression
      ast = parse("1 # This is a comment")

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

    def test_statement_list
      ast = parse("1; 2; 3;")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::NumericLiteral,
              value: 1
            }
          },
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::NumericLiteral,
              value: 2
            }
          },
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::NumericLiteral,
              value: 3
            }
          }
        ]
      }, ast)
    end

    def test_empty_statement
      ast = parse(";")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::EmptyStatement
          }
        ]
      }, ast)
    end
  end
end
