module Wid
  class ParserBlockStatementTest < ParserTest
    def test_block_statement_curly_braces
      ast = parse("{ 1; 2; 3 }")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::BlockStatement,
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
          }
        ]
      }, ast)
    end

    def test_block_statement_do_end
      ast = parse("do 1; 2; 3 end")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::BlockStatement,
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
          }
        ]
      }, ast)
    end

    def test_block_statement_empty
      ast = parse("{ }")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::BlockStatement,
            body: []
          }
        ]
      }, ast)
    end

    def test_block_statement_nested
      ast = parse("{ 1 { 2 } }")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::BlockStatement,
            body: [
              {
                class: Nodes::ExpressionStatement,
                expression: {
                  class: Nodes::NumericLiteral,
                  value: 1
                }
              },
              {
                class: Nodes::BlockStatement,
                body: [
                  {
                    class: Nodes::ExpressionStatement,
                    expression: {
                      class: Nodes::NumericLiteral,
                      value: 2
                    }
                  }
                ]
              }
            ]
          }
        ]
      }, ast)
    end
  end
end
