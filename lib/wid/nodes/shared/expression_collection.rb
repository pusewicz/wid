module Wid
  module Nodes
    module Shared
      module ExpressionCollection
        class_exec do
          attr_accessor :expressions
        end

        def initialize
          @expressions = []
        end

        def <<(expression)
          expressions << expression
        end

        def ==(other)
          expressions == other.expressions
        end

        def children
          expressions
        end
      end
    end
  end
end
