module Wid
  class ParserTest < Test
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

    def test_block_statement_curly_braces
      ast = parse("{ 1; 2; 3 }")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::BlockStatement,
            statements: [
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
            statements: [
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
            statements: []
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
            statements: [
              {
                class: Nodes::ExpressionStatement,
                expression: {
                  class: Nodes::NumericLiteral,
                  value: 1
                }
              },
              {
                class: Nodes::BlockStatement,
                statements: [
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

    def test_binary_expression_additive
      ast = parse("1 + 2")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "+",
              left: {
                class: Nodes::NumericLiteral,
                value: 1
              },
              right: {
                class: Nodes::NumericLiteral,
                value: 2
              }
            }
          }
        ]
      }, ast)
    end

    # left: 1 + 2
    # right: 3
    def test_binary_expression_chained
      ast = parse("1 + 2 - 3")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "-",
              left: {
                class: Nodes::BinaryExpression,
                operator: "+",
                left: {
                  class: Nodes::NumericLiteral,
                  value: 1
                },
                right: {
                  class: Nodes::NumericLiteral,
                  value: 2
                }
              },
              right: {
                class: Nodes::NumericLiteral,
                value: 3
              }
            }
          }
        ]
      }, ast)
    end

    def test_binary_expression_multiplicative
      ast = parse("1 * 2")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "*",
              left: {
                class: Nodes::NumericLiteral,
                value: 1
              },
              right: {
                class: Nodes::NumericLiteral,
                value: 2
              }
            }
          }
        ]
      }, ast)
    end

    def test_binary_expression_multiplicative_chained
      ast = parse("1 + 2 * 3")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "+",
              left: {
                class: Nodes::NumericLiteral,
                value: 1
              },
              right: {
                class: Nodes::BinaryExpression,
                operator: "*",
                left: {
                  class: Nodes::NumericLiteral,
                  value: 2
                },
                right: {
                  class: Nodes::NumericLiteral,
                  value: 3
                }
              }
            }
          }
        ]
      }, ast)
    end

    def test_binary_expression_multiplicative_chained_equal
      ast = parse("1 * 2 * 3")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "*",
              left: {
                class: Nodes::BinaryExpression,
                operator: "*",
                left: {
                  class: Nodes::NumericLiteral,
                  value: 1
                },
                right: {
                  class: Nodes::NumericLiteral,
                  value: 2
                }
              },
              right: {
                class: Nodes::NumericLiteral,
                value: 3
              }
            }
          }
        ]
      }, ast)
    end

    def test_binary_expression_grouped
      ast = parse("(1 + 2) * 3")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "*",
              left: {
                class: Nodes::BinaryExpression,
                operator: "+",
                left: {
                  class: Nodes::NumericLiteral,
                  value: 1
                },
                right: {
                  class: Nodes::NumericLiteral,
                  value: 2
                }
              },
              right: {
                class: Nodes::NumericLiteral,
                value: 3
              }
            }
          }
        ]
      }, ast)
    end

    def test_assignment_statement
      ast = parse("foo = 1")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::AssignmentExpression,
              operator: "=",
              left: {
                class: Nodes::Identifier,
                name: "foo"
              },
              right: {
                class: Nodes::NumericLiteral,
                value: 1
              }
            }
          }
        ]
      }, ast)
    end

    def test_assignment_statement_chained
      ast = parse("foo = bar = 1")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::AssignmentExpression,
              operator: "=",
              left: {
                class: Nodes::Identifier,
                name: "foo"
              },
              right: {
                class: Nodes::AssignmentExpression,
                operator: "=",
                left: {
                  class: Nodes::Identifier,
                  name: "bar"
                },
                right: {
                  class: Nodes::NumericLiteral,
                  value: 1
                }
              }
            }
          }
        ]
      }, ast)
    end

    def test_if_statement
      ast = parse(<<~WID)
        if x 
          x = 1
        else 
          x = 2
        end
      WID

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::IfStatement,
            test: {
              class: Nodes::Identifier,
              name: "x"
            },
            consequent: {
              class: Nodes::BlockStatement,
              statements: [
                {
                  class: Nodes::ExpressionStatement,
                  expression: {
                    class: Nodes::AssignmentExpression,
                    operator: "=",
                    left: {
                      class: Nodes::Identifier,
                      name: "x"
                    },
                    right: {
                      class: Nodes::NumericLiteral,
                      value: 1
                    }
                  }
                }
              ]
            },
            alternate: {
              class: Nodes::BlockStatement,
              statements: [
                {
                  class: Nodes::ExpressionStatement,
                  expression: {
                    class: Nodes::AssignmentExpression,
                    operator: "=",
                    left: {
                      class: Nodes::Identifier,
                      name: "x"
                    },
                    right: {
                      class: Nodes::NumericLiteral,
                      value: 2
                    }
                  }
                }
              ]
            }
          }
        ]
      }, ast)
    end

    private

    def assert_ast_equal(expected, actual)
      assert_equal(expected, ast_to_hash(actual))
      # assert_equal(expected[:class], actual.class)

      # expected.each do |key, value|
      #   next if key == :children || key == :class
      #   assert_respond_to(actual, key)
      #   assert_equal(value, actual.send(key))
      # end

      # if expected.has_key?(:children)
      #   expected[:children].each_with_index do |child, index|
      #     assert_ast_equal(child, actual.children[index])
      #   end
      # end
    end

    def ast_to_hash(ast)
      if ast.is_a?(Array)
        ast.map { |node| ast_to_hash(node) }
      else
        ast.to_hash
      end
    end

    def parse(input)
      tokens = Lexer.tokenize(input)
      Parser.parse(tokens)
    end

    def dump(ast, indent = 0, output = [])
      ast.children.each do |child|
        output << "#{" " * indent}#{child.inspect}"
        dump(child, indent + 2)
      end

      output.join("\n")
    end
  end
end
